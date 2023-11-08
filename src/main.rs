use headless_chrome::{Browser, Element, Tab};
use std::{
    env::var,
    sync::Arc,
};
use chrono::{Datelike, Duration, Utc, Weekday};
use dotenv::dotenv;
use ics::{Event, ICalendar, properties::{
    Summary,
    Location,
    Organizer,
    Description,
    DtStart,
    DtEnd,
}};
use rand::random;

#[derive(Debug)]
pub struct PlanningEventCollected {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub cours: String,
    pub salle: String,
    pub prof: String,
    pub groupe: Vec<String>,
    pub notes: String,
    pub week: u32,
}

#[derive(Debug, Clone)]
pub struct PlanningEvent {
    pub start: chrono::naive::NaiveDateTime,
    pub duration_s: Duration,
    pub cours: String,
    pub prof: String,
    pub salle: String,
    pub groupe: Vec<String>,
    pub notes: String,
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    let url = "https://cas.ut-capitole.fr/cas/login?service=https%3A%2F%2Fade-production.ut-capitole.fr%2Fdirect%2Fmyplanning.jsp";

    let events = connect_to_planning(url).await
        .expect("Failed to get planning");

    create_ics(&events).await
        .expect("Failed to create ics file");
}

async fn connect_to_planning(url: &str) -> Result<Vec<PlanningEvent>, String> {
    let browser = Browser::default()
        .expect("Failed to launch browser");

    let tab = browser.new_tab()
        .expect("Failed to create new tab");

    println!("Navigating to {}", url);
    tab.navigate_to(url)
        .expect("Failed to navigate");

    println!("Waiting for login page to load");
    tab.wait_for_element("input#username")
        .expect("Failed to find username field");

    println!("Typing username");
    tab.send_character(var("UT1_USERNAME")
        .expect("UT1_USERNAME not set")
        .as_str())
        .expect("Failed to type username")
        .press_key("Tab")
        .expect("Failed to press tab");

    println!("Typing password");
    tab.send_character(var("UT1_PASSWORD")
        .expect("UT1_PASSWORD not set")
        .as_str())
        .expect("Failed to type password")
        .press_key("Enter")
        .expect("Failed to press enter");

    println!("Redirecting to planning page");
    let binding = tab.wait_for_element("div.grilleData")
        .expect("Apparently failed to log in")
        .attributes
        .expect("Failed to get 'div.grilleData' attributes");

    // pre parse planning container style tag
    let planning_container = binding
        .get(11)
        .expect("Failed to get 'div.grilleData' style attribute")
        .split("hidden; ")
        .last().unwrap().to_string();

    // parse planning container style tag to get width and height
    let planning_container = planning_container
        .split("width: ")
        .last().unwrap()
        .split("px; height: ")
        .map(|s| parse_int(s))
        .collect::<Vec<_>>();

    // create a vec containing 5 tabs (parallelize scraping)
    println!("Creating new tabs");
    let mut handles = Vec::new();
    let mut week = chrono::Local::now().iso_week().week();
    let mut tabs = Vec::new();
    tabs.push(tab.clone());
    for _i in 1..5 {
        let new_tab = browser.new_tab()
            .expect("Failed to create new tab");
        tabs.push(new_tab);
    }

    for tab in tabs {
        let planning_container_clone = planning_container.clone();
        let url_clone = url.to_string();
        let handle = tokio::spawn(async move {
            let real_week = chrono::Local::now().iso_week().week();
            if (week - real_week) > 0 {
                println!("New thread for week {} navigating to url", week);
                tab.navigate_to(url_clone.as_str())
                    .expect("Failed to navigate");

                // wait for div.grilleData to load
                tab.wait_for_element("div.grilleData")
                    .expect("Failed to find element");

                // click on next week tab
                tab.find_element(format!("table#x-auto-{}", week - 1).as_str())
                    .expect("Failed to find element")
                    .click().unwrap();

                // wait for div.grilleData to load
                tab.wait_for_element("div.grilleData")
                    .expect("Failed to find element");
            }

            scrape_events(&tab, &planning_container_clone, &week).await
        });
        handles.push(handle);
        week += 1;
    }

    let mut all_events = Vec::new();

    for handle in handles {
        let events = handle.await
            .expect("Thread panicked")
            .expect("Failed to scrape events");
        all_events.extend(events);
    }

    /*let events = scrape_events(&tab, &planning_container).await
        .expect("Failed to scrape events");*/

    Ok(all_events)
}

async fn scrape_events(tab: &Arc<Tab>, planning_container: &Vec<i32>, week: &u32) -> Result<Vec<PlanningEvent>, String> {
    let mut events = Vec::new();

    println!("Scraping week {}", week);

    // put all elements in div.grilleData in a vector
    let formatted_events;
    match tab.find_elements("div.grilleData > div") {
        Ok(elements) => {
            println!("Parsing events for week {}", week);
            // add events of current week to PlanningEventCollected vec
            events.extend(parse_events(elements, &week).await
                .expect("Failed to parse events"));

            // convert PlanningEventCollected to PlanningEvent
            formatted_events = convert_events(&events, &planning_container).await
                .expect("Failed to convert events");
        }
        Err(_) => formatted_events = Vec::new(),
    };

    Ok(formatted_events)
}

async fn parse_events(html_events: Vec<Element<'_>>, week: &u32) -> Result<Vec<PlanningEventCollected>, String> {
    let mut parsed_events = Vec::new();

    for event in html_events {
        let event_position = event.attributes
            .clone()
            .ok_or("Failed to get 'planning event' attributes")?
            .get(1)
            .expect("Failed to get 'planning event' style attribute")
            .split("absolute; ")
            .last().unwrap()
            .split("left: ")
            .last().unwrap()
            .split("px; top: ")
            .map(|s| parse_int(s))
            .collect::<Vec<_>>();

        // get event size by selecting table.event child of div.event
        let event_size = event.find_element("table.event")
            .expect("Failed to find 'table.event' element")
            .attributes
            .expect("Failed to get 'table.event' attributes");

        // collect Vec of width and height of table.event
        let event_size = event_size
            .get(3)
            .expect("Failed to get 'table.event' style attribute")
            .split("width:")
            .last().unwrap()
            .split("px;height:")
            .map(|s| parse_int(s))
            .collect::<Vec<_>>();

        // get event datas by selecting div.eventText child of div.event
        let event_datas = event.find_element("div.eventText")
            .expect("Failed to find 'div.eventText' element")
            .get_content().unwrap();
        let mut event_datas = event_datas
            .split("eventText\">")
            .last().unwrap()
            .split("</b><br>")
            .collect::<Vec<_>>();

        // parses the 2nd element of vec into multiple elements
        event_datas = event_datas.into_iter()
            .flat_map(|s| s.split("<br>").collect::<Vec<_>>())
            .collect();

        // remove last element of vec ("</div>")
        event_datas.pop();

        // set event datas in struct and push it in parsed_events
        parsed_events.push(PlanningEventCollected {
            x: event_position[0],
            y: event_position[1],
            width: event_size[0],
            height: event_size[1],
            cours: event_datas.remove(0).to_string(),
            salle: event_datas.remove(0).to_string(),
            prof: event_datas.remove(0).to_string(),
            notes: event_datas.pop().unwrap().to_string(),
            groupe: event_datas.into_iter()
                .map(|s| s.to_string())
                .collect(),
            week: *week,
        });
    }

    Ok(parsed_events)
}

async fn convert_events(events: &Vec<PlanningEventCollected>, planning_container: &Vec<i32>) -> Result<Vec<PlanningEvent>, String> {
    let mut converted_events = Vec::new();

    // store today 7 am in date format
    let date = chrono::Local::now().date_naive()
        .and_hms_opt(7, 0, 0).unwrap();

    let date = match date.weekday() {
        Weekday::Mon => date,
        _ => date - Duration::days(Weekday::num_days_from_monday(&date.weekday()) as i64),
    };

    // loop on vec of events to convert element positions to date and duration
    for event in events {
        // get px height of half hour and day
        let half_hour_in_px = planning_container[1] as f32 / 28.0; // from 7 to 21, 14 hours -> 28 half hours
        let day_in_px = planning_container[0] as f32 / 6.0; // 6 days
        // calculate days overflow if event is in next week
        let week_overflow = (event.week - chrono::Local::now().iso_week().week()) * 7;
        // get start date of event (monday 7 am + x days + y half hours)
        let start = date
            + Duration::days(event.x as i64 / day_in_px as i64)
            + Duration::minutes((event.y as i64 / half_hour_in_px as i64) * 30)
            - Duration::hours(1)                    // -1 hour because of timezone
            + Duration::days(week_overflow as i64); // + weeks if event is in next week
        // get duration of event (event.height in px / half hours in px * 30 minutes)
        let duration_s = Duration::minutes((event.height as i64 / half_hour_in_px as i64) * 30);

        // add event to vec of converted events
        converted_events.push(PlanningEvent {
            start,
            duration_s,
            cours: event.cours.clone(),
            prof: event.prof.clone(),
            salle: event.salle.clone(),
            groupe: event.groupe.clone(),
            notes: event.notes.clone(),
        });
    }

    Ok(converted_events)
}

async fn create_ics(events: &Vec<PlanningEvent>) -> Result<&str, std::io::Error> {
    let mut calendar = ICalendar::new("2.0", "https://www.github.com/loouis-t/ut1-timetable");

    // loop over events to create ics events
    for event in events {
        // generate random uid
        let uid = format!("{}", random::<i64>());

        // create ics event
        let mut ics_event = Event::new(uid, Utc::now().format("%Y%m%dT%H%M%SZ").to_string());
        ics_event.push(DtStart::new(event.clone().start.format("%Y%m%dT%H%M%SZ").to_string()));
        ics_event.push(DtEnd::new((event.clone().start + event.clone().duration_s).format("%Y%m%dT%H%M%SZ").to_string()));
        ics_event.push(Summary::new(event.clone().cours));
        ics_event.push(Location::new(event.clone().salle));
        ics_event.push(Organizer::new(event.clone().prof));
        ics_event.push(Description::new(event.clone().notes));

        // Add it to calendar
        calendar.add_event(ics_event);
    }

    // Save ics file in directory
    calendar.save_file("ut1.ics")?;

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