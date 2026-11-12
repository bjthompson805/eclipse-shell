//! Current weather and today's sunrise/sunset for the launcher. The location
//! is the configured place (looked up by name) or else the IP address's city
//! (ip-api.com); conditions come from Open-Meteo. Fetched with curl and
//! refreshed every 15 minutes.

use std::cell::RefCell;
use std::time::Duration;

use gtk::{gio, glib};
use gtk4 as gtk;
use serde::Deserialize;

use crate::config;

const REFRESH: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
pub struct Weather {
    /// e.g. "12°C".
    pub temp: String,
    pub condition: &'static str,
    pub glyph: Glyph,
    pub city: String,
    /// Seconds after local midnight.
    pub sunrise: i32,
    pub sunset: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Glyph {
    Sun,
    Cloud,
    Rain,
    Snow,
    Fog,
    Storm,
}

thread_local! {
    static CURRENT: RefCell<Option<Weather>> = RefCell::default();
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = RefCell::default();
}

pub fn current() -> Option<Weather> {
    CURRENT.with(|c| c.borrow().clone())
}

pub fn on_change(f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

/// Starts fetching now and then on a timer.
pub fn start() {
    glib::spawn_future_local(async {
        loop {
            match fetch().await {
                Ok(weather) => {
                    CURRENT.with(|c| *c.borrow_mut() = Some(weather));
                    LISTENERS.with(|l| l.borrow().iter().for_each(|f| f()));
                }
                Err(err) => eprintln!("eclipse-shell: weather unavailable: {err}"),
            }
            glib::timeout_future(REFRESH).await;
        }
    });
}

#[derive(Deserialize)]
struct Location {
    city: String,
    lat: f64,
    lon: f64,
}

#[derive(Deserialize)]
struct Forecast {
    current: Current,
    daily: Daily,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: f64,
    weather_code: u32,
}

#[derive(Deserialize)]
struct Daily {
    sunrise: Vec<String>,
    sunset: Vec<String>,
}

async fn curl(url: &str) -> Result<String, glib::Error> {
    let process = gio::Subprocess::newv(
        &["curl".as_ref(), "-s".as_ref(), "--max-time".as_ref(), "10".as_ref(), url.as_ref()],
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )?;
    let (stdout, _) = process.communicate_utf8_future(None).await?;
    Ok(stdout.map(String::from).unwrap_or_default())
}

#[derive(Deserialize)]
struct Places {
    #[serde(default)]
    results: Vec<Place>,
}

#[derive(Deserialize)]
struct Place {
    name: String,
    admin1: Option<String>,
    country: Option<String>,
    latitude: f64,
    longitude: f64,
}

/// Looks up "Town, Region" by name, preferring a match on the region (state,
/// province or country) when one is given. Results come most populous first.
async fn geocode(query: &str) -> Result<Location, Box<dyn std::error::Error>> {
    let (town, region) = match query.split_once(',') {
        Some((town, region)) => (town.trim(), Some(region.trim().to_lowercase())),
        None => (query.trim(), None),
    };
    let url = format!("https://geocoding-api.open-meteo.com/v1/search?count=10&format=json&name={}", glib::Uri::escape_string(town, None, false));
    let places: Places = serde_json::from_str(&curl(&url).await?)?;
    let in_region = |p: &&Place| {
        region.as_ref().is_none_or(|r| [&p.admin1, &p.country].iter().any(|field| field.as_ref().is_some_and(|f| f.to_lowercase() == *r)))
    };
    let place = places.results.iter().find(in_region).or(places.results.first()).ok_or("place not found")?;
    Ok(Location { city: place.name.clone(), lat: place.latitude, lon: place.longitude })
}

async fn fetch() -> Result<Weather, Box<dyn std::error::Error>> {
    let location = match config::WEATHER_LOCATION {
        "" => serde_json::from_str(&curl("http://ip-api.com/json/?fields=city,lat,lon").await?)?,
        place => geocode(place).await?,
    };
    let unit = if uses_fahrenheit() { "fahrenheit" } else { "celsius" };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current=temperature_2m,weather_code&daily=sunrise,sunset&forecast_days=1&timezone=auto&temperature_unit={unit}",
        location.lat, location.lon
    );
    let forecast: Forecast = serde_json::from_str(&curl(&url).await?)?;
    let (condition, glyph) = describe(forecast.current.weather_code);
    let first = |times: &[String]| times.first().and_then(|t| seconds_of_day(t)).ok_or("missing sun times");
    Ok(Weather {
        temp: format!("{}°{}", forecast.current.temperature_2m.round(), if unit == "fahrenheit" { 'F' } else { 'C' }),
        condition,
        glyph,
        city: location.city,
        sunrise: first(&forecast.daily.sunrise)?,
        sunset: first(&forecast.daily.sunset)?,
    })
}

/// Open-Meteo returns local times like "2026-11-11T07:02".
fn seconds_of_day(time: &str) -> Option<i32> {
    let clock = time.split('T').nth(1)?;
    let mut parts = clock.split(':').map(|p| p.parse::<i32>().ok());
    Some(parts.next()?? * 3600 + parts.next()?? * 60)
}

/// The US and a couple of others measure in Fahrenheit; go by the locale.
fn uses_fahrenheit() -> bool {
    let locale = ["LC_MEASUREMENT", "LC_ALL", "LANG"].iter().find_map(|v| std::env::var(v).ok().filter(|l| !l.is_empty())).unwrap_or_default();
    ["_US", "_LR", "_MM"].iter().any(|region| locale.contains(region))
}

/// WMO weather interpretation codes, as Open-Meteo reports them.
fn describe(code: u32) -> (&'static str, Glyph) {
    match code {
        0 => ("Clear", Glyph::Sun),
        1 => ("Mostly clear", Glyph::Sun),
        2 => ("Partly cloudy", Glyph::Cloud),
        3 => ("Cloudy", Glyph::Cloud),
        45 | 48 => ("Fog", Glyph::Fog),
        51..=57 => ("Drizzle", Glyph::Rain),
        61..=67 => ("Rain", Glyph::Rain),
        71..=77 => ("Snow", Glyph::Snow),
        80..=82 => ("Showers", Glyph::Rain),
        85 | 86 => ("Snow showers", Glyph::Snow),
        95..=99 => ("Thunderstorm", Glyph::Storm),
        _ => ("Unknown", Glyph::Cloud),
    }
}

/// Stroked outline icons, 24-unit grid. Sun, cloud, rain, snow and storm are
/// Feather icons (MIT licence, feathericons.com).
fn svg_path(glyph: Glyph) -> &'static str {
    match glyph {
        Glyph::Sun => "M16 12a4 4 0 1 1-8 0a4 4 0 1 1 8 0 M12 1v2 M12 21v2 M4.22 4.22l1.42 1.42 M18.36 18.36l1.42 1.42 M1 12h2 M21 12h2 M4.22 19.78l1.42-1.42 M18.36 5.64l1.42-1.42",
        Glyph::Cloud => "M18 10h-1.26A8 8 0 1 0 9 20h9a5 5 0 0 0 0-10z",
        Glyph::Rain => "M16 13v8 M8 13v8 M12 15v8 M20 16.58A5 5 0 0 0 18 7h-1.26A8 8 0 1 0 4 15.25",
        Glyph::Snow => "M20 17.58A5 5 0 0 0 18 8h-1.26A8 8 0 1 0 4 16.25 M8 16h.01 M8 20h.01 M12 18h.01 M12 22h.01 M16 16h.01 M16 20h.01",
        Glyph::Fog => "M4 9h16 M4 13h16 M7 17h10",
        Glyph::Storm => "M19 16.9A5 5 0 0 0 18 7h-1.26a8 8 0 1 0-11.62 9 M13 11l-4 6h6l-4 6",
    }
}

/// The glyph as a texture `px` pixels square, stroked in `color`.
pub fn glyph_texture(glyph: Glyph, px: i32, color: &str, stroke: f64) -> Option<gtk::gdk::Texture> {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{px}" height="{px}" viewBox="0 0 24 24" fill="none" stroke="{color}" stroke-width="{stroke}" stroke-linecap="round" stroke-linejoin="round"><path d="{}"/></svg>"#,
        svg_path(glyph)
    );
    gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(svg.into_bytes())).ok()
}

/// Where the sun or moon sits within its current phase: `(fraction, is_day)`.
/// Day runs sunrise..sunset; night runs sunset..next sunrise across midnight.
pub fn sky_position(now: i32, sunrise: i32, sunset: i32) -> (f64, bool) {
    const DAY: i32 = 86_400;
    if sunset <= sunrise {
        return (f64::from(now) / f64::from(DAY), (6 * 3600..20 * 3600).contains(&now));
    }
    if (sunrise..sunset).contains(&now) {
        return (f64::from(now - sunrise) / f64::from(sunset - sunrise), true);
    }
    let night = DAY - (sunset - sunrise);
    let into = if now >= sunset { now - sunset } else { now + DAY - sunset };
    (f64::from(into) / f64::from(night), false)
}
