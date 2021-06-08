use chrono::{SubsecRound, Timelike, Utc};
use clap::{App, Arg};
use rand::Rng;
use reqwest::header;
use reqwest::header::HeaderValue;
use reqwest::Client;
use serde::Deserialize;
use slog::{debug, error, info, o, warn, Drain, Logger};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::{Debug, Formatter};
use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::sleep;

struct NotificationConfig {
    telegram_chat_id: String,
    telegram_token: String,
}

impl Debug for NotificationConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationConfig")
            .field("telegram_chat_id", &self.telegram_chat_id)
            .field("telegram_token", &"**************")
            .finish()
    }
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
struct Module {
    #[serde(rename = "type")]
    type_: String, //knownPatient
    services: Vec<u64>,
}

#[derive(Deserialize, Debug)]
struct ModuleItems {
    items: Vec<Module>,
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

#[derive(Debug)]
struct Config {
    latitude: f64,
    longitude: f64,
    radius: u64,
    notification_config: NotificationConfig,
}

type Date = String;
type ServiceMap = HashMap<u64, HashSet<Date>>;
type DoctorMap = HashMap<String, ServiceMap>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log = init_logger();

    dotenv::dotenv().ok();

    let config = get_config(&log);

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

    send_start_info(&log, &config, &client).await;

    let mut doctor_map: DoctorMap = HashMap::new();
    loop {
        match check_services(&log, &config, &client, &mut doctor_map).await {
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

async fn send_start_info(log: &Logger, config: &Config, client: &Client) {
    let text = format!(
        "Starting Version 0.0.4 (Only mRNA) at {}/{}, {}km radius",
        config.latitude, config.longitude, config.radius
    );
    info!(log, "{}", text);

    send_text(client, config, &text, log).await;
}

async fn check_services(
    log: &Logger,
    config: &Config,
    client: &Client,
    notification_map: &mut DoctorMap,
) -> Result<Option<Vec<Appointment>>, Box<dyn std::error::Error>> {
    let mut appointments = Vec::new();
    let mut notification_map_new = HashMap::new();

    let relevant_doctor_info = client
        .get("https://www.jameda.de/mannheim/corona-impftermine/spezialisten/")
        .query(&[
            ("ajaxparams[0]", "add|popular|otb_status"),
            (
                "ajaxparams[1]",
                &format!(
                    "change|geoball|{}_{}_{}",
                    config.latitude, config.longitude, config.radius
                ),
            ),
            ("output", "json"),
        ])
        .send()
        .await?
        .json::<DoctorInfoResult>()
        .await?;

    for doctor_info in relevant_doctor_info.results {
        debug!(
            log,
            "Checking {}, {}km", doctor_info.name_kurz, doctor_info.entfernung
        );

        let offers_vaccination = doctor_info
            .services
            .iter()
            .any(|entry| entry.to_lowercase().contains("corona-impfung"));

        if !offers_vaccination {
            debug!(
                log,
                "{} does not offer any vaccination", doctor_info.name_kurz
            );
            continue;
        }

        // Be nice and slow down
        sleep(Duration::from_millis(2000)).await;

        let services_for_patients = client
            .get(format!(
                "https://booking-service.jameda.de/public/config/modules?refId={}",
                doctor_info.ref_id
            ))
            .send()
            .await?
            .json::<ModuleItems>()
            .await
            .map(|module_items| {
                module_items
                    .items
                    .iter()
                    .find(|&module| module.type_ == "knownPatient")
                    .map(|a| a.services.clone())
                    .unwrap_or_else(Vec::new)
            })
            .unwrap_or_else(|_| Vec::new());

        debug!(
            log,
            "Services {:?} are reserved for existing patients", services_for_patients
        );

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
            let title_lower = service.title.to_lowercase();
            if !title_lower.contains("impfung")
                || !title_lower.contains("corona")
                || title_lower.contains("zweit")
            {
                continue;
            }
            if !(title_lower.contains("biontech")
                || title_lower.contains("pfizer")
                || title_lower.contains("moderna"))
            {
                continue;
            }
            if services_for_patients.contains(&service.id) {
                debug!(
                    log,
                    "Skipping {} as it is reserved for patients", service.title
                );
                continue;
            }

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
                // Check if the date for the service/appointment id was already reported as available, if not, add it and add to return values
                let notification_entries = notification_map
                    .entry(doctor_info.ref_id.clone())
                    .or_insert_with(HashMap::new)
                    .entry(appointment.service_id)
                    .or_insert_with(HashSet::new);
                if notification_entries.insert(date.clone()) {
                    // Wasn't reported yet nor is it in the new return value, add it
                    appointment.dates.push(date.clone());
                }

                // Every entry must be added to the map of the current run. This one will be used for comparision in the next run
                // (relevant e.g. if old map contained dates that aren't available in the new run, but might be available later again)
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
    info!(log, "NEW: {:?}", notification_map_new);
    *notification_map = notification_map_new;

    if appointments.is_empty() {
        Ok(None)
    } else {
        Ok(Some(appointments))
    }
}

async fn notify(log: &Logger, appointments: &[Appointment], config: &Config, client: &Client) {
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

    send_text(client, config, &text, log).await;
}

async fn send_text(client: &Client, config: &Config, text: &str, log: &Logger) {
    match client
        .post(format!(
            "https://api.telegram.org/bot{}/sendMessage",
            config.notification_config.telegram_token
        ))
        .query(&[
            ("chat_id", &config.notification_config.telegram_chat_id),
            ("text", &text.to_owned()),
        ])
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
            .use_custom_timestamp(|f: &mut dyn Write| {
                write!(f, "{}", Utc::now().round_subsecs(3).to_rfc3339())
            })
            .build(),
    )
    .fuse();
    slog::Logger::root(drain, o!())
}

fn get_config(log: &Logger) -> Config {
    let matches = App::new("Jameda Impfhelper")
        .version("0.0.3")
        .arg(
            Arg::new("latitude")
                .long("lat")
                .value_name("COORDINATE")
                .about("Sets the latitude of the search start point, e.g. 49.1234567")
                .required(true),
        )
        .arg(
            Arg::new("longitude")
                .long("long")
                .value_name("COORDINATE")
                .about("Sets the longitude of the search start point, e.g. 8.9876543")
                .required(true),
        )
        .arg(
            Arg::new("radius")
                .long("radius")
                .value_name("NUMBER")
                .about("Sets the search radius, e.g. 100")
                .default_value("100"),
        )
        .get_matches();

    let latitude = matches
        .value_of_t("latitude")
        .expect("Latitude isn't a number");
    let longitude = matches
        .value_of_t("longitude")
        .expect("Longitude isn't a number");
    let radius = matches.value_of_t("radius").expect("Radius isn't a number");

    let notification_config = NotificationConfig {
        telegram_chat_id: std::env::var("TELEGRAM_CHAT_ID")
            .expect("TELEGRAM_CHAT_ID env var must be set"),
        telegram_token: std::env::var("TELEGRAM_TOKEN")
            .expect("TELEGRAM_TOKEN env var must be set"),
    };

    let config = Config {
        latitude,
        longitude,
        radius,
        notification_config,
    };
    debug!(log, "Using Config: {:?}", config);

    config
}
