use rand::Rng;
use reqwest::Client;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;
use tokio::time::sleep;

struct NotificationConfig {
    telegram_chat_id: String,
    telegram_token: String,
}

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

impl From<Offering> for Appointment {
    fn from(offering: Offering) -> Self {
        Self::new(offering.id, offering.title)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting Version V0.0.3");

    let config = NotificationConfig {
        telegram_chat_id: std::env::var("TELEGRAM_CHAT_ID").expect("TELEGRAM_CHAT_ID must be set"),
        telegram_token: std::env::var("TELEGRAM_TOKEN").expect("TELEGRAM_TOKEN must be set"),
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
) -> Result<Option<Vec<Appointment>>, Box<dyn std::error::Error>> {
    let mut appointments = Vec::new();

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

async fn notify(appointments: &[Appointment], config: &NotificationConfig, client: &Client) {
    let text = appointments.iter().map(|a| {
        format!("{}: {}", a.title, a.dates.join(","))
    }).collect::<Vec<String>>().join("\n");

    match client
        .post(format!("https://api.telegram.org/bot{}/sendMessage", config.telegram_token))
        .query(&[
            ("chat_id", &config.telegram_chat_id),
            ("text", &text),
        ])
        .send()
        .await
    {
        Ok(resp) => {
            println!("Sent with status code: {}", resp.status());
        }
        Err(e) => {
            println!("Error during sending: {:?}", e)
        }
    }
}
