use rand::Rng;
use reqwest::Client;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::ops::{Deref, DerefMut};
use std::time::Duration;
use tokio::time::sleep;

#[derive(Deserialize, Debug)]
struct Offering {
    id: u64,
    title: String,
}

#[derive(Deserialize, Debug)]
struct Slot {
    slot: String,
}

#[derive(Debug)]
struct Appointment {
    id: u64,
    title: String,
    dates: Vec<String>,
}

impl Appointment {
    pub fn new(id: u64, title: String) -> Self {
        Appointment {
            id,
            title,
            dates: Vec::new(),
        }
    }

    pub fn is_bookable(&self) -> bool {
        !self.dates.is_empty()
    }
}

impl Display for Appointment {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.title, self.dates.join(","))
    }
}

impl From<Offering> for Appointment {
    fn from(offering: Offering) -> Self {
        Self::new(offering.id, offering.title)
    }
}

struct Appointments(Vec<Appointment>);
impl Display for Appointments {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.0.iter().map(|a| a.to_string()).collect::<Vec<String>>().join(" || "))
    }
}

impl Deref for Appointments {
    type Target = Vec<Appointment>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Appointments {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

struct NotificationConfig {
    twilio_sid: String,
    twilio_token: String,
    phone_to: String,
    phone_from: String,
    sms_endpoint: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting Version V0.0.2");

    let config = NotificationConfig {
        twilio_sid: std::env::var("TWILIO_SID").unwrap(),
        twilio_token: std::env::var("TWILIO_TOKEN").unwrap(),
        phone_to: std::env::var("PHONE_TO").unwrap(),
        phone_from: std::env::var("PHONE_FROM").unwrap(),
        sms_endpoint: format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
            std::env::var("TWILIO_SID").unwrap()
        ),
    };

    let client = reqwest::Client::new();
    let mut notification_map: HashMap<u64, HashSet<String>> = HashMap::new();
    loop {
        match check_offerings(&mut notification_map).await {
            Ok(option) => match option {
                None => {
                    println!("Nothing new...");
                }
                Some(appointments) => {
                    notify(&appointments, &config, &client).await;
                }
            },
            Err(e) => {
                println!("Error: {:?}", e);
            }
        }
        let random_sec = rand::thread_rng().gen_range(180..600);
        sleep(Duration::from_secs(random_sec)).await;
    }
}

async fn check_offerings(
    notification_map: &mut HashMap<u64, HashSet<String>>,
) -> Result<Option<Appointments>, Box<dyn std::error::Error>> {
    let mut appointments = Appointments(Vec::new());

    let offerings =
        reqwest::get("https://booking-service.jameda.de/public/resources/81229096/services")
            .await?
            .json::<Vec<Offering>>()
            .await?;

    for offering in offerings {
        if !offering.title.to_lowercase().contains("impfung") {
            continue;
        }
        println!("Checking {}", offering.title);

        let slots = match reqwest::get(format!(
            "https://booking-service.jameda.de/public/resources/81229096/slots?serviceId={}",
            offering.id
        ))
        .await?
        .json::<Vec<Slot>>()
        .await {
            Ok(slots) => {
                if slots.is_empty() {
                    println!("No available slots. Skip");
                    continue;
                } else {
                    slots
                }
            },
            Err(e) => {
                println!("Skipping due to error {:?}", e);
                continue;
            }
        };

        // Format is 2021-05-29T10:15:00+02:00
        let dates = slots
            .into_iter()
            .map(|s| s.slot[..s.slot.find('T').unwrap_or_else(|| s.slot.len())].to_owned())
            .collect::<BTreeSet<String>>();

        let mut appointment: Appointment = offering.into();
        for date in dates {
            // Check if the date for the offering/appointment id was already reported as available, if not, add it
            let notification_entries = notification_map
                .entry(appointment.id)
                .or_insert_with(HashSet::new);
            if notification_entries.insert(date.clone()) {
                // Wasn't reported yet, add it
                appointment.dates.push(date);
            }
        }

        // Only add Appointments where at least one date is available and not reported yet
        if appointment.is_bookable() {
            appointments.push(appointment);
        }
    }

    if appointments.is_empty() {
        Ok(None)
    } else {
        Ok(Some(appointments))
    }
}

async fn notify(appointments: &Appointments, config: &NotificationConfig, client: &Client) {
    match client
        .post(&config.sms_endpoint)
        .basic_auth(&config.twilio_sid, Some(&config.twilio_token))
        .form(&[
            ("To", &config.phone_to),
            ("From", &config.phone_from),
            ("Body", &appointments.to_string()),
        ])
        .send()
        .await
    {
        Ok(resp) => {
            println!("Sent notification string: {}", appointments.to_string());
            println!("Got status code: {}", resp.status());
        }
        Err(e) => {
            println!("Error during sending: {:?}", e)
        }
    }
}
