use headless_chrome::{Browser, Element, LaunchOptions, Tab};
use std::{
    env::var,
    sync::Arc,
};
use chrono::{Datelike, Duration, Utc, Weekday};
use dotenv::dotenv;
use headless_chrome::{
    browser::{
        default_executable,
        transport::{
            SessionId,
            Transport,
        },
    },
    protocol::cdp::Fetch::{
        events::RequestPausedEvent,
        FulfillRequest,
    },
};
use headless_chrome::browser::tab::{
    RequestInterceptor,
    RequestPausedDecision,
};

use ics::{Event, ICalendar, properties::{
    Summary,
    Location,
    Organizer,
    Description,
    DtStart,
    DtEnd,
}};
use rand::random;
use anyhow::{Result, anyhow, Context};

#[derive(Debug, Clone)]
pub struct PlanningEvent {
    pub start: chrono::naive::NaiveDateTime,
    pub duration_s: Duration,
    pub cours: String,
    pub prof: String,
    pub salle: String,
    pub notes: String,
}

struct CssInterceptor;

impl RequestInterceptor for CssInterceptor {
    fn intercept(
        &self,
        _transport: Arc<Transport>,
        _session_id: SessionId,
        event: RequestPausedEvent,
    ) -> RequestPausedDecision {
        let filetype = event.params.request.headers.0.expect("No headers");
        if !filetype.get("Accept").map_or(
            false,
            |s| s.to_string().contains("text/css"),
        ) {
            return RequestPausedDecision::Continue(None);
        }
        RequestPausedDecision::Fulfill(FulfillRequest {
            request_id: event.params.request_id,
            response_code: 200,
            response_headers: None,
            binary_response_headers: None,
            body: None,
            response_phrase: None,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    let url = "https://cas.ut-capitole.fr/cas/login?service=https%3A%2F%2Fade-production.ut-capitole.fr%2Fdirect%2Fmyplanning.jsp";

    let current_millis = chrono::Local::now().timestamp_millis();
    let events = connect_to_planning(url).await?;
    let new_millis = chrono::Local::now().timestamp_millis();
    println!("Scraping took {} ms", new_millis - current_millis);

    create_ics(&events).await?;

    Ok(())
}

async fn connect_to_planning(url: &str) -> Result<Vec<PlanningEvent>> {
    let mut chrome_args = Vec::new();
    chrome_args.push(std::ffi::OsStr::new("--blink-settings=imagesEnabled=false"));

    let launch_options = LaunchOptions::default_builder()
        .path(Some(default_executable().map_err(|e| anyhow!(e))?))
        .enable_gpu(true)
        .args(chrome_args)
        .build()?;

    println!("{:#?}", launch_options.path);

    let browser = Browser::new(launch_options)?; // Launch headless Chrome browser

    let tab = browser.new_tab()?;               // Create a new tab

    // Intercept CSS files and refuse them
    let css_request_interceptor: Arc<dyn RequestInterceptor + Send + Sync> =
        Arc::new(CssInterceptor);
    tab.enable_fetch(None, None)?;
    tab.enable_request_interception(css_request_interceptor.clone())?;

    println!("Navigating to {}", url);
    tab.navigate_to(url)?;

    println!("Waiting for login page to load");
    tab.wait_for_element("input#username")?;

    println!("Typing username");
    tab.send_character(var("UT1_USERNAME")?
        .as_str())?
        .press_key("Tab")?;

    println!("Typing password");
    tab.send_character(var("UT1_PASSWORD")?
        .as_str())?
        .press_key("Enter")?;

    println!("Redirecting to planning page");
    let binding = tab.wait_for_element("div.grilleData")?
        .attributes
        .context("Failed to get 'planning container' attributes")?;

    // pre parse planning container style tag
    let planning_container = binding
        .get(11)
        .context("Failed to get 'planning container' style attribute")?
        .split("hidden; ").last()
        .context("Failed to get 'planning container' style attribute")?
        .to_string();

    // parse planning container style tag to get width and height
    let planning_container = planning_container
        .split("width: ").last()
        .context("Failed to get 'planning container' style attribute")?
        .split("px; height: ")
        .map(|s| parse_int(s))
        .collect::<Vec<_>>();

    // create a vec containing 5 tabs (parallelize scraping)
    println!("Creating new tabs");
    let mut handles = Vec::new();
    let mut week = chrono::Local::now().iso_week().week();
    let mut tabs = Vec::new();
    tabs.push(tab.clone());
    let nb_weeks_to_scrape = var("NB_WEEKS_TO_SCRAPE")?
        .parse::<i8>()
        .context("Failed to parse 'NB_WEEKS_TO_SCRAPE'")?;
    for _i in 1..nb_weeks_to_scrape {
        // create new tabs and enable css interception
        let new_tab = browser.new_tab()?;
        new_tab.enable_fetch(None, None)?;
        new_tab.enable_request_interception(css_request_interceptor.clone())?;

        tabs.push(new_tab);
    }

    for tab in tabs {
        let planning_container_clone = planning_container.clone();
        let url_clone = url.to_string();
        let handle = tokio::spawn(async move {
            let real_week = chrono::Local::now().iso_week().week();
            if (week - real_week) > 0 {
                println!("New thread for week {} navigating to url", week % 52 + 1);
                tab.navigate_to(url_clone.as_str())?;

                // wait for div.grilleData to load
                tab.wait_for_element("div.grilleData")?;

                // click on next week tab
                tab.find_element(format!("table#x-auto-{}", week - 1).as_str())?
                    .find_element("button.x-btn-text")?
                    .click()?;

                // wait for div.grilleData to load
                tab.wait_for_element("div.grilleData")?;
            }

            scrape_events(&tab, &planning_container_clone, &week).await
        });
        handles.push(handle);
        week += 1;
    }

    let mut all_events = Vec::new();
    for handle in handles {
        all_events.extend(handle.await??);
    }

    Ok(all_events)
}

async fn scrape_events(
    tab: &Arc<Tab>,
    planning_container: &Vec<i32>,
    week: &u32,
) -> Result<Vec<PlanningEvent>> {
    let mut events = Vec::new();

    println!("Scraping week {}", week % 52 + 1);

    // put all elements in div.grilleData in a vector. null vec if no elements
    match tab.find_elements("div.grilleData > div.event") {
        Ok(elements) => {
            println!("Parsing events for week {}", week);
            // add events of current week to PlanningEvent vec
            let mut threads = Vec::new();
            for element in elements {
                let thread = tokio::spawn(async move {
                    parse_events(&element, week, planning_container).await
                });

                threads.push(thread);
            }

            for thread in threads {
                events.push(thread.await??);
            }

            /*events.extend(parse_events(
                elements,
                &week,
                planning_container,
            ).await?);*/
        }
        _ => {}
    };

    Ok(events)
}

async fn parse_events(
    event: &Element<'_>,
    week: &u32,
    planning_container: &Vec<i32>,
) -> Result<PlanningEvent> {

    let event_position = event.attributes
        .clone()
        .context("Failed to get 'div.event' attributes")?
        .get(1).unwrap()
        .split("absolute; ")
        .last()
        .context("Failed to split 'div.event' style attribute ('absolute; ')")?
        .split("left: ")
        .last()
        .context("Failed to split 'div.event' style attribute ('left: ')")?
        .split("px; top: ")
        .map(|s| parse_int(s))
        .collect::<Vec<_>>();

    // get event size by selecting table.event child of div.event
    let event_height = event.find_element("table.event")?
        .attributes
        .context("Failed to get 'table.event' attributes")?;

    // collect height of table.event (necessary to calculate event duration)
    let event_height = event_height
        .get(3)
        .context("Failed to get 'table.event' style attribute")?
        .split("height:")
        .last()
        .context("Failed to split 'table.event' style attribute ('height:')")?;

    // get event datas by selecting div.eventText child of div.event
    let event_datas = event.find_element("div.eventText")?
        .get_content()
        .context("Failed to get 'div.eventText' content")?;
    let mut event_datas = event_datas
        .split("eventText\">")
        .last()
        .context("Failed to split 'div.eventText' content ('eventText\">')")?
        .split("</b><br>")
        .collect::<Vec<_>>();

    // parses the 2nd element of vec into multiple elements
    event_datas = event_datas.into_iter()
        .flat_map(|s| s.split("<br>").collect::<Vec<_>>())
        .collect();

    // remove last element of vec ("</div>")
    event_datas.pop();

    // convert event position/width/height to start hour and duration
    let (start, duration_s) = convert_events(
        event_position[0],
        event_position[1],
        parse_int(event_height),
        &planning_container,
        week,
    ).await?;

    Ok(PlanningEvent {
        start,
        duration_s,
        cours: event_datas.remove(0).to_string(),
        salle: event_datas.remove(0).to_string(),
        prof: event_datas.remove(0).to_string(),
        notes: event_datas.pop().unwrap().to_string(),
    })

}


async fn convert_events(
    x: i32,
    y: i32,
    height: i32,
    planning_container: &Vec<i32>,
    week: &u32,
) -> Result<(chrono::naive::NaiveDateTime, Duration)> {

    // store today 7 am in date format
    let date = chrono::Local::now().date_naive()
        .and_hms_opt(7, 0, 0).unwrap();

    let date = match date.weekday() {
        Weekday::Mon => date,
        _ => date - Duration::days(Weekday::num_days_from_monday(&date.weekday()) as i64),
    };

    // get px height of half hour and day
    let half_hour_in_px = planning_container[1] as f32 / 28.0; // from 7 to 21, 14 hours -> 28 half hours
    let day_in_px = planning_container[0] as f32 / 7.0; // 7 days
    // calculate days overflow if event is in next week
    let week_overflow = (week - chrono::Local::now().iso_week().week()) * 7;
    // get start date of event (monday 7 am + x days + y half hours)
    let start = date
        + Duration::days(x as i64 / day_in_px as i64)
        + Duration::minutes((y as i64 / half_hour_in_px as i64) * 30)
        - Duration::hours(1)                    // -1 hour because of timezone
        + Duration::days(week_overflow as i64); // + weeks if event is in next week
    // get duration of event (event.height in px / half hours in px * 30 minutes)
    let duration_s = Duration::minutes((height as i64 / half_hour_in_px as i64) * 30);

    Ok((start, duration_s))
}

async fn create_ics(events: &Vec<PlanningEvent>) -> Result<&str> {
    println!("Creating ics file from merged events");

    // create ics calendar
    let mut calendar = ICalendar::new(
        "2.0",
        "https://www.github.com/loouis-t/ut1-timetable",
    );

    let mut handles = Vec::new();
    for event in events.clone() {
        println!("new thread for event");
        let handle = tokio::spawn(async move {
            // create random uid
            let uid = format!("{}", random::<i64>());

            // create ics event
            let mut ics_event = Event::new(
                uid,
                Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
            );
            ics_event.push(DtStart::new(
                event.start.format("%Y%m%dT%H%M%SZ").to_string())
            );
            ics_event.push(DtEnd::new(
                (event.start + event.duration_s)
                    .format("%Y%m%dT%H%M%SZ").to_string())
            );
            ics_event.push(Summary::new(event.cours));
            ics_event.push(Location::new(event.salle));
            ics_event.push(Organizer::new(event.prof));
            ics_event.push(Description::new(event.notes));

            // return event
            ics_event
        });
        handles.push(handle);
    }

    for handle in handles {
        calendar.add_event(handle.await?);
    }

    // loop over events to create ics events
    /*for event in events {
        // create random uid
        let uid = format!("{}", random::<i64>());

        // create ics event
        let mut ics_event = Event::new(
            uid,
            Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
        );
        ics_event.push(DtStart::new(
            event.start.format("%Y%m%dT%H%M%SZ").to_string())
        );
        ics_event.push(DtEnd::new(
            (event.start + event.duration_s)
                .format("%Y%m%dT%H%M%SZ").to_string())
        );
        ics_event.push(Summary::new(&event.cours));
        ics_event.push(Location::new(&event.salle));
        ics_event.push(Organizer::new(&event.prof));
        ics_event.push(Description::new(&event.notes));

        // Add it to calendar
        calendar.add_event(ics_event);
    }*/

    // Save ics file in directory
    calendar.save_file("ut1.ics")?;

    if var("PROD")? == "true".to_string() {
        // scp ics file to server
        std::process::Command::new("scp")
            .arg("ut1.ics")
            .arg(format!(
                "{}:{}",
                var("SERVER_IP")?,
                var("PATH_TO_DEPLOY_ICS")?
            ))
            .spawn()?;
    } else {
        std::fs::copy("ut1.ics", var("PATH_TO_DEPLOY_ICS")?)?;
    }

    Ok("ICS saved in directory")
}

// just converts string to i32 and removes "px;" if present
fn parse_int(s: &str) -> i32 {
    if s.contains("px") {
        s.split("px")
            .next().unwrap()
            .parse::<i32>()
            .unwrap()
    } else {
        s.parse::<i32>().unwrap()
    }
}