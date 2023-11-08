use headless_chrome::{Browser, Element};
use std::env::var;
use dotenv::dotenv;

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
}

pub struct PlanningEvent {
    pub start: i64,
    pub duration: i8,
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

    let (planning_container, events) = get_raw_planning(url).await
        .expect("Failed to get planning");


    println!("Planning container: {:#?}", planning_container);
    println!("Events: {:#?}", events[0]);
}

async fn get_raw_planning(url: &str) -> Result<(Vec<i32>, Vec<PlanningEventCollected>), String> {
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

    // put all elements in div.grilleData in a vector
    let html_planning_events = tab.find_elements("div.grilleData > div")
        .expect("Failed to find elements");

    // parse planning events
    let events = parse_events(&html_planning_events).await
        .expect("Failed to parse planning events");

    Ok((planning_container, events))
}

async fn parse_events(html_events: &Vec<Element<'_>>) -> Result<Vec<PlanningEventCollected>, String> {

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
        });
    }

    Ok(parsed_events)
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