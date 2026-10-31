//! Open-Meteo provider: current conditions + daily forecast for one location.
//! Implements the WeatherSource shape. Free API, no key — the right default
//! for a self-hosted server (TRMNL's #1 plugin is weather; their plugin needs
//! a Tempest station or WeatherAPI key, we need only coordinates).
//!
//! Config: { latitude: f64, longitude: f64, units?: "imperial" | "metric" }
//! Snapshot payload (normalized, units already applied at request time):
//!   { "location": name, "current": {temp, feels, humidity, wind, code, time},
//!     "days": [ {date, code, hi, lo, precip}, ... ], "fetched_at": iso }

use super::{Provider, Snapshot};
use serde_json::{json, Value};
use std::pin::Pin;

pub struct OpenMeteo;

/// WMO weather code -> short label (the set Open-Meteo actually emits).
pub fn wmo_label(code: i64) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 => "Light drizzle",
        53 => "Drizzle",
        55 => "Heavy drizzle",
        56 | 57 => "Freezing drizzle",
        61 => "Light rain",
        63 => "Rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 => "Light snow",
        73 => "Snow",
        75 => "Heavy snow",
        77 => "Snow grains",
        80 => "Light showers",
        81 => "Showers",
        82 => "Heavy showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm, hail",
        _ => "Unknown",
    }
}

fn validate(config: &Value) -> anyhow::Result<()> {
    for key in ["latitude", "longitude"] {
        let v = config.get(key).and_then(|v| v.as_f64())
            .ok_or_else(|| anyhow::anyhow!("config.{key} must be a number"))?;
        let ok = match key {
            "latitude" => (-90.0..=90.0).contains(&v),
            _ => (-180.0..=180.0).contains(&v),
        };
        if !ok {
            anyhow::bail!("config.{key} out of range: {v}");
        }
    }
    match config.get("units").and_then(|v| v.as_str()) {
        None | Some("imperial") | Some("metric") => Ok(()),
        Some(other) => anyhow::bail!("config.units must be \"imperial\" or \"metric\", got \"{other}\""),
    }
}

impl Provider for OpenMeteo {
    fn kind(&self) -> &'static str {
        "open_meteo"
    }

    fn shape(&self) -> &'static str {
        "weather"
    }

    fn validate(&self, config: &Value) -> anyhow::Result<()> {
        validate(config)
    }

    fn fetch(&self, config: Value)
        -> Pin<Box<dyn std::future::Future<Output = anyhow::Result<Snapshot>> + Send>>
    {
        Box::pin(async move {
            validate(&config)?;
            let imperial = config.get("units").and_then(|v| v.as_str()) != Some("metric");
            let lat = config["latitude"].as_f64().unwrap();
            let lon = config["longitude"].as_f64().unwrap();
            let mut url = format!(
                "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
                 &current=temperature_2m,relative_humidity_2m,apparent_temperature,wind_speed_10m,weather_code\
                 &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max\
                 &timezone=America%2FLos_Angeles&forecast_days=4");
            if imperial {
                url.push_str("&temperature_unit=fahrenheit&wind_speed_unit=mph");
            }
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?;
            let resp = client.get(&url).send().await?.error_for_status()?;
            let raw: Value = resp.json().await?;

            let cur = &raw["current"];
            let daily = &raw["daily"];
            let days: Vec<Value> = daily["time"].as_array().map(|ts| ts.iter().enumerate().map(|(i, t)| json!({
                "date": t.as_str().unwrap_or(""),
                "code": daily["weather_code"][i].as_i64().unwrap_or(-1),
                "label": wmo_label(daily["weather_code"][i].as_i64().unwrap_or(-1)),
                "hi": daily["temperature_2m_max"][i].as_f64().unwrap_or(0.0).round(),
                "lo": daily["temperature_2m_min"][i].as_f64().unwrap_or(0.0).round(),
                "precip": daily["precipitation_probability_max"][i].as_i64().unwrap_or(0),
            })).collect()).unwrap_or_default();

            let code = cur["weather_code"].as_i64().unwrap_or(-1);
            Ok(Snapshot::Weather(json!({
                "location": config.get("name").and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("{:.2}, {:.2}", lat, lon)),
                "units": if imperial { "F" } else { "C" },
                "current": {
                    "temp": cur["temperature_2m"].as_f64().unwrap_or(0.0).round(),
                    "feels": cur["apparent_temperature"].as_f64().unwrap_or(0.0).round(),
                    "humidity": cur["relative_humidity_2m"].as_i64().unwrap_or(0),
                    "wind": cur["wind_speed_10m"].as_f64().unwrap_or(0.0).round(),
                    "code": code,
                    "label": wmo_label(code),
                    "time": cur["time"].as_str().unwrap_or(""),
                },
                "days": days,
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_bounds() {
        assert!(validate(&json!({"latitude": 48.4, "longitude": -122.3})).is_ok());
        assert!(validate(&json!({"latitude": 91.0, "longitude": 0.0})).is_err());
        assert!(validate(&json!({"latitude": 0.0})).is_err());
        assert!(validate(&json!({"latitude": 0.0, "longitude": 0.0, "units": "metric"})).is_ok());
        assert!(validate(&json!({"latitude": 0.0, "longitude": 0.0, "units": "kelvin"})).is_err());
    }

    #[test]
    fn wmo_labels_cover_the_real_set() {
        assert_eq!(wmo_label(0), "Clear");
        assert_eq!(wmo_label(3), "Overcast");
        assert_eq!(wmo_label(63), "Rain");
        assert_eq!(wmo_label(95), "Thunderstorm");
        assert_eq!(wmo_label(12345), "Unknown");
    }
}
