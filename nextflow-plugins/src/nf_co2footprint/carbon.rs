//! The carbon intensity of nf-co2footprint: a fixed value, or live values from the Electricity
//! Maps API every hour, which each task gets as the mean over its run time.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// The latest carbon intensity of a zone, by Electricity Maps.
const LATEST: &str = "https://api.electricitymap.org/v3/carbon-intensity/latest";

/// How often the live value is asked for.
const HOURLY: Duration = Duration::from_secs(3600);

/// How long one request may take.
const LIMIT: Duration = Duration::from_secs(30);

/// The values of carbon intensity in gCO₂e/kWh, each from its time in milliseconds since 1970
/// until the next one.
pub(super) struct Intensity {
    points: Arc<Mutex<Vec<(i64, f64)>>>,
    /// Ends the worker that asks for live values; dropping it ends the worker too.
    stop: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Intensity {
    pub(super) fn fixed(value: f64) -> Intensity {
        Intensity {
            points: Arc::new(Mutex::new(vec![(i64::MIN, value)])),
            stop: None,
            worker: None,
        }
    }

    /// Starts with `value`, then asks Electricity Maps for the value of `zone` now and every
    /// hour, on a thread of its own.
    pub(super) fn live(value: f64, zone: String, token: String) -> Intensity {
        let mut intensity = Intensity::fixed(value);
        let points = intensity.points.clone();
        let (stop, stopped) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("nf-co2footprint".into())
            .spawn(move || {
                loop {
                    match fetch(LATEST, &zone, &token) {
                        Ok((time, value)) => {
                            tracing::info!(
                                "[nf-co2footprint] API call successful. CI: {value} gCO\u{2082}e/kWh \
                                 ({time}). Response code: 200 (OK)."
                            );
                            if let Ok(mut points) = points.lock() {
                                points.push((time, value));
                            }
                        }
                        Err(error) => tracing::warn!("[nf-co2footprint] {error}"),
                    }
                    if !matches!(stopped.recv_timeout(HOURLY), Err(mpsc::RecvTimeoutError::Timeout)) {
                        break;
                    }
                }
            });
        match worker {
            Ok(worker) => {
                intensity.stop = Some(stop);
                intensity.worker = Some(worker);
            }
            Err(error) => tracing::warn!(%error, "the carbon intensity is not asked for live"),
        }
        intensity
    }

    /// Stops asking for live values, and leaves a request that runs behind.
    pub(super) fn stop(&mut self) {
        self.stop.take();
        self.worker.take();
    }

    /// The mean of the values in force from `start` to `end`, weighted by time; the value at
    /// `start` when the span is empty.
    pub(super) fn between(&self, start: Option<i64>, end: Option<i64>) -> f64 {
        let Ok(points) = self.points.lock() else {
            return 0.0;
        };
        let at = |time: i64| {
            (points.iter().rev())
                .find(|(from, _)| *from <= time)
                .or(points.first())
                .map_or(0.0, |(_, value)| *value)
        };
        let (Some(start), Some(end)) = (start, end) else {
            return at(start.or(end).unwrap_or(i64::MAX));
        };
        if end <= start {
            return at(start);
        }
        let mut cuts: Vec<i64> = (points.iter())
            .map(|(from, _)| *from)
            .filter(|from| *from > start && *from < end)
            .collect();
        cuts.insert(0, start);
        cuts.push(end);
        let total: f64 = (cuts.windows(2))
            .map(|span| at(span[0]) * (span[1] - span[0]) as f64)
            .sum();
        total / (end - start) as f64
    }

    /// The live values with their times, for the report.
    pub(super) fn records(&self) -> Vec<(i64, f64)> {
        let points = self
            .points
            .lock()
            .map(|points| points.clone())
            .unwrap_or_default();
        points
            .into_iter()
            .filter(|(time, _)| *time != i64::MIN)
            .collect()
    }
}

/// The latest value of a zone: its time in milliseconds since 1970 and the intensity.
fn fetch(address: &str, zone: &str, token: &str) -> Result<(i64, f64), String> {
    let client = storage::http_client()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let (status, body) = runtime.block_on(async {
        let request = (client.get(format!("{address}?zone={zone}")).timeout(LIMIT))
            .header("auth-token", token);
        let response = request.send().await.map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|error| error.to_string())?;
        Ok::<_, String>((status, body))
    })?;
    if status != 200 {
        return Err(format!(
            "API call failed. Response code: {status} -> {body}"
        ));
    }
    let answer: serde_json::Value =
        serde_json::from_str(&body).map_err(|error| error.to_string())?;
    let value = (answer["carbonIntensity"].as_f64())
        .ok_or_else(|| format!("API call failed. Response code: {status} -> {body}"))?;
    let time = (answer["datetime"].as_str())
        .and_then(|time| time.parse::<jiff::Timestamp>().ok())
        .map_or_else(crate::report_format::now, |time| time.as_millisecond());
    Ok((time, value))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn weighs_by_time() {
        let intensity = Intensity::fixed(100.0);
        if let Ok(mut points) = intensity.points.lock() {
            points.push((1000, 300.0));
        }
        assert_eq!(intensity.between(Some(0), Some(2000)), 200.0);
        assert_eq!(intensity.between(Some(1500), Some(1500)), 300.0);
        assert_eq!(intensity.between(Some(0), None), 100.0);
    }

    /// Answers one request like Electricity Maps, and gives the request line and headers.
    fn serve(status: &str, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let address = format!(
            "http://{}/latest",
            listener.local_addr().expect("an address")
        );
        let status = status.to_owned();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("a request");
            let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
            let mut request = String::new();
            while reader.read_line(&mut request).is_ok_and(|read| read > 2) {}
            let mut stream = stream;
            let answer = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(answer.as_bytes()).expect("answers");
            request
        });
        (address, server)
    }

    #[test]
    fn asks_electricity_maps() {
        let body = r#"{"zone":"DE","carbonIntensity":312,"datetime":"2026-11-06T09:00:00.000Z"}"#;
        let (address, server) = serve("200 OK", body);
        let answer = fetch(&address, "DE", "secret-token");
        let request = server.join().expect("the server ends").to_lowercase();
        assert_eq!(answer, Ok((1_793_955_600_000, 312.0)));
        assert!(
            request.starts_with("get /latest?zone=de http/1.1"),
            "{request}"
        );
        assert!(request.contains("auth-token: secret-token"), "{request}");
        let (address, server) = serve("401 Unauthorized", "no access");
        let answer = fetch(&address, "DE", "wrong");
        server.join().expect("the server ends");
        assert_eq!(
            answer,
            Err("API call failed. Response code: 401 -> no access".into())
        );
    }
}
