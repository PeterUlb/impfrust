use chrono::{Timelike, Utc};
use rand::Rng;
use reqwest::header;
use reqwest::header::HeaderValue;
use reqwest::Client;
use serde::Deserialize;
use slog::{debug, error, info, o, warn, Drain, Logger};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::sleep;

struct NotificationConfig {
    telegram_chat_id: String,
    telegram_token: String,
}

#[derive(Deserialize, Debug)]
struct DoctorInfoResult {
    results: Vec<DoctorInfo>,
}

#[derive(Deserialize, Debug)]
struct DoctorInfo {
    ref_id: String,
    name_kurz: String,
    entfernung: f64,
    services: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct Service {
    id: u64,
    title: String,
}

#[derive(Deserialize, Debug)]
struct StatusCode {
    code: u64,
    message: String,
}

#[derive(Deserialize, Debug)]
struct Slot {
    slot: String,
}

#[derive(Debug)]
struct Appointment {
    doc_id: String,
    doc_name: String,
    distance: f64,
    service_id: u64,
    service_title: String,
    dates: Vec<String>,
}

impl Appointment {
    pub fn new(
        doc_id: String,
        doc_name: String,
        distance: f64,
        service_id: u64,
        service_title: String,
    ) -> Self {
        Appointment {
            doc_id,
            doc_name,
            distance,
            service_id,
            service_title,
            dates: Vec::new(),
        }
    }

    pub fn is_bookable(&self) -> bool {
        !self.dates.is_empty()
    }
}

type Date = String;
type ServiceMap = HashMap<u64, HashSet<Date>>;
type DoctorMap = HashMap<String, ServiceMap>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log = init_logger();

    info!(log, "Starting Version V0.0.3");
    dotenv::dotenv().ok();

    let config = NotificationConfig {
        telegram_chat_id: std::env::var("TELEGRAM_CHAT_ID").expect("TELEGRAM_CHAT_ID must be set"),
        telegram_token: std::env::var("TELEGRAM_TOKEN").expect("TELEGRAM_TOKEN must be set"),
    };

    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::USER_AGENT,
        HeaderValue::from_static(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:88.0) Gecko/20100101 Firefox/88.0",
        ),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();

    let mut doctor_map: DoctorMap = HashMap::new();
    loop {
        match check_services(&log, &client, &mut doctor_map).await {
            Ok(option) => match option {
                None => {
                    info!(log, "No changes compared to last run");
                }
                Some(appointments) => {
                    notify(&log, &appointments, &config, &client).await;
                }
            },
            Err(e) => {
                error!(log, "Error: {:?}", e);
            }
        }
        let hour = chrono::Utc::now().hour();
        let random_sec = if hour >= 22 || hour <= 3 {
            rand::thread_rng().gen_range(20 * 60..50 * 60)
        } else {
            rand::thread_rng().gen_range(5 * 60..10 * 60)
        };
        sleep(Duration::from_secs(random_sec)).await;
    }
}

async fn check_services(
    log: &Logger,
    client: &Client,
    notification_map: &mut DoctorMap,
) -> Result<Option<Vec<Appointment>>, Box<dyn std::error::Error>> {
    let mut appointments = Vec::new();
    let mut notification_map_new = HashMap::new();

    //https://www.jameda.de/heidelberg/corona-impftermine/spezialisten/?ajaxparams[0]=add|popular|otb_status&ajaxparams[1]=change|geoball|49.39875_8.672434_100&output=json
    let relevant_doctor_info = client
        .get("https://www.jameda.de/heidelberg/corona-impftermine/spezialisten/")
        .query(&[
            ("ajaxparams[0]", "add|popular|otb_status"),
            ("ajaxparams[1]", "change|geoball|49.39875_8.672434_150"),
            ("output", "json"),
        ])
        .send()
        .await?
        .json::<DoctorInfoResult>()
        .await?;

    for doctor_info in relevant_doctor_info.results {
        if !doctor_info.services.contains(&"Corona-Impfung".to_owned()) {
            continue;
        }

        let services = match client
            .get(format!(
                "https://booking-service.jameda.de/public/resources/{}/services",
                doctor_info.ref_id
            ))
            .send()
            .await?
            .json::<Vec<Service>>()
            .await
        {
            Ok(srv) => srv,
            Err(e) => {
                // E.g. The specified refId (1234) does not have OTB available.
                // Shouldn't happen anymore since we use "otb_status"
                warn!(
                    log,
                    "Skipping {}, no appointment bookable ({})", doctor_info.name_kurz, e
                );
                continue;
            }
        };

        for service in services {
            if !service.title.to_lowercase().contains("impfung")
                || service.title.to_lowercase().contains("zweit")
            {
                continue;
            }
            // Be nice and slow down
            sleep(Duration::from_millis(2000)).await;
            info!(
                log,
                "Checking {} from {} ({}km)",
                service.title,
                doctor_info.name_kurz,
                doctor_info.entfernung
            );

            let slots: Vec<Slot> = match client
                .get(format!(
                    "https://booking-service.jameda.de/public/resources/{}/slots?serviceId={}",
                    doctor_info.ref_id, service.id
                ))
                .send()
                .await?
                .text()
                .await
            {
                Ok(response_string) => match serde_json::from_str(&response_string) {
                    Ok(slots) => slots,
                    Err(e) => {
                        // Also possible: {"code":2000,"message":"There are no open slots, because all slots have been booked already."}
                        let status: StatusCode =
                            serde_json::from_str(&response_string).unwrap_or(StatusCode {
                                code: 500,
                                message: e.to_string(),
                            });
                        info!(log, "{:?}", status);
                        continue;
                    }
                },
                Err(e) => {
                    error!(log, "Skipping due to error {:?}", e);
                    continue;
                }
            };

            // Format is 2021-05-29T10:15:00+02:00
            let dates = slots
                .into_iter()
                .map(|s| s.slot[..s.slot.find('T').unwrap_or_else(|| s.slot.len())].to_owned())
                .collect::<BTreeSet<String>>();

            let mut appointment = Appointment::new(
                doctor_info.ref_id.clone(),
                doctor_info.name_kurz.to_owned(),
                doctor_info.entfernung,
                service.id,
                service.title,
            );
            for date in dates {
                // Check if the date for the service/appointment id was already reported as available, if not, add it
                let notification_entries = notification_map
                    .entry(doctor_info.ref_id.clone())
                    .or_insert_with(HashMap::new)
                    .entry(appointment.service_id)
                    .or_insert_with(HashSet::new);
                if notification_entries.insert(date.clone()) {
                    // Wasn't reported yet, add it
                    appointment.dates.push(date.clone());
                }

                notification_map_new
                    .entry(doctor_info.ref_id.clone())
                    .or_insert_with(HashMap::new)
                    .entry(appointment.service_id)
                    .or_insert_with(|| {
                        let mut set = HashSet::new();
                        set.insert(date.clone());
                        set
                    })
                    .insert(date.clone());
            }

            // Only add Appointments where at least one date is available and not reported yet
            if appointment.is_bookable() {
                appointments.push(appointment);
            }
        }
    }

    // Set all found entries as old entries, so new ones can be reported
    info!(log, "OLD: {:?}", notification_map);
    info!(log, "NEW: {:?}", notification_map_new);
    *notification_map = notification_map_new;

    if appointments.is_empty() {
        Ok(None)
    } else {
        Ok(Some(appointments))
    }
}

async fn notify(
    log: &Logger,
    appointments: &[Appointment],
    config: &NotificationConfig,
    client: &Client,
) {
    let text = appointments
        .iter()
        .map(|a| {
            format!(
                "{} ({}, {}km): {}",
                a.service_title,
                a.doc_name,
                a.distance,
                a.dates.join(",")
            )
        })
        .collect::<Vec<String>>()
        .join("\n");

    info!(log, "Sending: {}", text);

    match client
        .post(format!(
            "https://api.telegram.org/bot{}/sendMessage",
            config.telegram_token
        ))
        .query(&[("chat_id", &config.telegram_chat_id), ("text", &text)])
        .send()
        .await
    {
        Ok(resp) => {
            info!(log, "Sent with status code: {}", resp.status());
        }
        Err(e) => {
            error!(log, "Error during sending: {:?}", e)
        }
    }
}

fn init_logger() -> Logger {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = Mutex::new(
        slog_term::FullFormat::new(decorator)
            .use_custom_timestamp(|f: &mut dyn Write| write!(f, "{}", Utc::now().to_string()))
            .build(),
    )
    .fuse();
    slog::Logger::root(drain, o!())
}
