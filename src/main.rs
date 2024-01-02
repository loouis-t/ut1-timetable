/*
 * Copyright (c) 2024. All rights reserved.
 * This software is the confidential and proprietary information of Louis Travaux ("Confidential Information").
 * You shall not disclose such Confidential Information and shall use it only in accordance with the terms of the license agreement you entered into with Louis Travaux.
 */

use headless_chrome::{Browser, LaunchOptions};
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
use tokio::task::JoinHandle;

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

    // scrape planning every 6 hours
    loop {
        let current_millis = chrono::Local::now().timestamp_millis();

        // Connect to planning and scrape events
        match scrape_ut1_planning(url).await {
            Ok(events) => {
                let new_millis = chrono::Local::now().timestamp_millis();
                println!("Scraping took {} ms", new_millis - current_millis);

                // Convert events to ics file
                create_ics_from_planning_event_vec(&events).await?;

                // deploy ics file
                deploy_ics_file().await?;

                // Wait 6 hour
                println!("Done. Next run at: {}", chrono::Local::now() + Duration::hours(6));
                tokio::time::sleep(tokio::time::Duration::from_secs(60 * 60 * 6)).await;
            },
            Err(e) => println!("INFO: {}", e),
        };

    }

}

async fn scrape_ut1_planning(url: &str) -> Result<Vec<PlanningEvent>> {
    let mut chrome_args = Vec::new();
    chrome_args.push(std::ffi::OsStr::new("--blink-settings=imagesEnabled=false"));

    let launch_options = LaunchOptions::default_builder()
        .path(Some(default_executable().map_err(|e| anyhow!(e))?))
        .enable_gpu(true)
        .sandbox(false)
        .args(chrome_args)
        .build()?;
    
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

    print!("Redirecting to planning page... ");
    let binding = match tab.wait_for_element("div.grilleData") {
        Ok(binding) => {
            println!();
            binding
                .attributes
                .context("Failed to get 'planning container' attributes")?
        },
        Err(_) => {
            println!("Failed. Reloading to retry.");
            tab.reload(false, None)?;
            tab.wait_for_element("div.grilleData")?
                .attributes
                .context("Failed to get 'planning container' attributes")?
        },
    };

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
    let mut threads: Vec<JoinHandle<Result<Vec<PlanningEvent>>>> = Vec::new();
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
        let planning_container = planning_container.clone();
        let url = url.to_string();

        let thread = tokio::spawn(async move {
            let real_week = chrono::Local::now().iso_week().week();
            let mut elements_attributes_vec = Vec::new();

            if (week - real_week) > 0 {
                println!("New thread (week {}) navigating to url", week);
                tab.navigate_to(url.as_str())?;

                // click on next week tab
                let buttons = tab.wait_for_elements("button.x-btn-text")?;
                for button in buttons {
                    if button.get_inner_text()?.contains(format!("({})", week).as_str()) {
                        button.click()?;
                        break;
                    }
                }
            }

            match tab.wait_for_elements("div.grilleData > div") {
                Ok(html_el) => {
                    let (mut event_height, mut event_data, mut event_style);

                    for el in html_el {
                        // get event's height
                        event_height = el
                            .find_element("table.event")?
                            .get_attribute_value("style")?
                            .context("Failed to get 'table.event' style attribute")?;

                        // get event's data
                        event_data = el
                            .find_element("div.eventText")?
                            .get_content()?;

                        // get event's position
                        event_style = el.get_attribute_value("style")?
                            .context("Failed to get 'div.event' style attribute")?;

                        elements_attributes_vec.push((event_style, event_height, event_data));
                    }
                }
                Err(_) => {}, // POSSIBLY: no event for this week
            }

            match get_raw_planning_events(elements_attributes_vec, planning_container, &week).await {
                Ok(events) => Ok(events),
                Err(e) => {
                    println!("INFO: {}", e);
                    Ok(Vec::new())
                }
            }
        });
        threads.push(thread);
        week += 1;
    }

    let mut merged_events_vectors = Vec::new();
    for thread in threads {
        match thread.await? {
            Ok(events) => {
                if !events.is_empty() {
                    merged_events_vectors.extend(events)
                }
            },
            Err(e) => println!("INFO: {}", e),
        }
    }

    Ok(merged_events_vectors)
}

async fn get_raw_planning_events(
    elements: Vec<(String, String, String)>,
    planning_container: Vec<i32>,
    week: &u32,
) -> Result<Vec<PlanningEvent>> {
    // handle no elements for current week
    if elements.is_empty() {
        return Err(anyhow!("No event for week {}", week));
    }

    println!("Getting raw planning events for week {}", week);

    let mut events = Vec::new();
    for element in elements {
        events.push(parse_event(&element, &week, &planning_container).await?)
    }

    Ok(events)
}


async fn parse_event(
    (event, event_height, event_data): &(String, String, String),
    week: &u32,
    planning_container: &Vec<i32>,
) -> Result<PlanningEvent> {
    let event_position = event
        .split("absolute; ")
        .last()
        .context("Failed to split 'div.event' style attribute ('absolute; ')")?
        .split("left: ")
        .last()
        .context("Failed to split 'div.event' style attribute ('left: ')")?
        .split("px; top: ")
        .map(|s| parse_int(s))
        .collect::<Vec<_>>();

    // collect height of table.event (necessary to calculate event duration)
    let event_height = event_height
        .split("height:")
        .last()
        .context("Failed to split 'table.event' style attribute ('height:')")?;

    let mut event_data = event_data
        .split("eventText\">")
        .last()
        .context("Failed to split 'div.eventText' content ('eventText\">')")?
        .split("</b><br>")
        .collect::<Vec<_>>();

    // parses the 2nd element of vec into multiple elements
    event_data = event_data.into_iter()
        .flat_map(|s| s.split("<br>").collect::<Vec<_>>())
        .collect();

    // remove last element of vec ("</div>")
    event_data.pop();

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
        cours: event_data.remove(0).to_string(),
        salle: event_data.remove(0).to_string(),
        prof: event_data.remove(0).to_string(),
        notes: event_data.pop().unwrap().replace("\n", " "),
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

async fn create_ics_from_planning_event_vec(events: &Vec<PlanningEvent>) -> Result<&str> {
    println!("Creating ics file from merged events");

    // create ics calendar
    let mut calendar = ICalendar::new(
        "2.0",
        "https://www.github.com/loouis-t/ut1-timetable",
    );

    let mut threads = Vec::new();
    for event in events.clone() {
        let thread = tokio::spawn(async move {
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
        threads.push(thread);
    }

    for thread in threads {
        calendar.add_event(thread.await?);
    }

    // Save ics file in directory
    calendar.save_file("ut1.ics")?;

    Ok("ICS saved in directory")
}

async fn deploy_ics_file() -> Result<&'static str> {
    println!("Deploying ics file");
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
        match std::fs::copy("ut1.ics", var("PATH_TO_DEPLOY_ICS")?) {
            Ok(_) => {},
            Err(_) => println!("INFO: Running inside docker container, ics file not copied"),
        }
    }

    Ok("ICS deployed")
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