use rand::Rng;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting Version V0.0.1");

    let twilio_sid = std::env::var("TWILIO_SID").unwrap();
    let twilio_token = std::env::var("TWILIO_TOKEN").unwrap();
    let phone_to = std::env::var("PHONE_TO").unwrap();
    let phone_from = std::env::var("PHONE_FROM").unwrap();

    let sms_endpoint = format!("https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json", twilio_sid);

    let client = reqwest::Client::new();
    let mut notification_map: HashMap<u64, HashSet<String>> = HashMap::new();

    loop {
        match check_offerings(&mut notification_map).await {
            Ok(option) => match option {
                None => {
                    println!("Nothing new...");
                }
                Some(notification_str) => {
                    match client
                        .post(&sms_endpoint)
                        .basic_auth(&twilio_sid, Some(&twilio_token))
                        .form(&[("To", &phone_to), ("From", &phone_from), ("Body", &notification_str)])
                        .send()
                        .await {
                        Ok(resp) => {
                            println!("Sent notification string: {}", notification_str);
                            println!("Got status code: {}", resp.status()); }
                        Err(e) => {
                            println!("Error during sending: {:?}", e)
                        }
                    }
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
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let offerings =
        reqwest::get("https://booking-service.jameda.de/public/resources/81229096/services")
            .await?
            .json::<Vec<Offering>>()
            .await?;

    let mut notification_text = String::new();

    for offering in offerings {
        if offering.title.to_lowercase().contains("impfung") {
            println!("Checking {}", offering.title);
            let slots = reqwest::get(format!(
                "https://booking-service.jameda.de/public/resources/81229096/slots?serviceId={}",
                offering.id
            ))
                .await?
                .json::<Vec<Slot>>()
                .await;

            let slots = match slots {
                Ok(slots) => slots,
                Err(e) => {
                    println!("Skipping due to error {:?}", e);
                    continue;
                }
            };

            if !slots.is_empty() {
                let dates = slots
                    .into_iter()
                    .map(|s| s.slot[..s.slot.find("T").unwrap_or(s.slot.len())].to_owned())
                    .collect::<BTreeSet<String>>();

                let mut output = String::new();
                for date in dates {
                    let notification_entries = notification_map
                        .entry(offering.id)
                        .or_insert(HashSet::new());
                    if !notification_entries.contains(&date) {
                        if !output.is_empty() {
                            output.push(',');
                        } else {
                            output.push_str(&offering.title);
                            output.push_str(": ");
                        }
                        output.push_str(&date)
                    }
                    notification_entries.insert(date);
                    notification_text.push_str(&output);
                }
            }
        }
    }

    if notification_text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(notification_text))
    }
}
