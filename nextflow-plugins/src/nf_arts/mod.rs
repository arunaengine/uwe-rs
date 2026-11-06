//! nf-arts 1.0.0: draws each finished run from its tasks, as an SVG or PNG picture.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod draw;
mod image;
mod raster;

use std::path::Path;

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer, Start, Task};
use draw::{Mark, Scene};
use image::{Color, Picture};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["arts"],
    observer: Some(activate),
    ..Plugin::new("nf-arts", Kind::Observer, State::Implemented)
};

const STYLES: [&str; 8] = [
    "radial",
    "hilbert",
    "ridgeline",
    "phyllotaxis",
    "flowfield",
    "mondrian",
    "tree",
    "ginkgo",
];

/// The palettes: the background first, then the colours.
const PALETTES: [(&str, &[&str]); 7] = [
    (
        "viridis",
        &[
            "#0b0b12", "#5b2a86", "#3b528b", "#21918c", "#5ec962", "#fde725",
        ],
    ),
    (
        "magma",
        &[
            "#000004", "#5c1a8f", "#8c2981", "#de4968", "#fe9f6d", "#fcfdbf",
        ],
    ),
    (
        "nfcore",
        &[
            "#1b1d1e", "#24b064", "#9fd36a", "#ecdc86", "#f9a440", "#e35a2c",
        ],
    ),
    (
        "ocean",
        &[
            "#03111f", "#155e8b", "#2a8fc4", "#4fb9e0", "#93dcec", "#e0f7fa",
        ],
    ),
    (
        "sunset",
        &[
            "#1a0b1e", "#5c2a9d", "#b5179e", "#f72585", "#ff8c42", "#ffd166",
        ],
    ),
    (
        "mono",
        &["#f6f4ef", "#111111", "#3a3a3a", "#6b6b6b", "#9c9c9c"],
    ),
    (
        "mondrian",
        &[
            "#111111", "#f2efe6", "#d62e24", "#f2efe6", "#1d4f9c", "#f2efe6", "#f2efe6", "#f2c81f",
            "#f2efe6",
        ],
    ),
];

/// The `arts` scope, checked.
struct Settings {
    file: Option<String>,
    style: String,
    palette: String,
    colors: Option<Vec<Color>>,
    background: Option<Color>,
    width: u32,
    height: u32,
    title: bool,
}

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let value = |key: &str| {
        config
            .get(&["arts", key])
            .filter(|it| !matches!(it, Value::Null))
    };
    let flag = |key: &str| match value(key) {
        None => Ok(true),
        Some(Value::Boolean(flag)) => Ok(flag),
        Some(other) => match other.to_string().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            text => Err(format!("arts.{key} must be true or false, got '{text}'")),
        },
    };
    let whole = |key: &str| match value(key) {
        None => Ok(1200),
        Some(other) => (other.to_string().parse::<i64>())
            .map_err(|_| format!("arts.{key} must be a whole number, got '{other}'")),
    };
    if !flag("enabled")? {
        return Ok(None);
    }
    let style = value("style").map_or_else(|| "radial".into(), |it| it.to_string());
    if !STYLES.contains(&style.as_str()) {
        return Err(format!(
            "Unknown arts.style '{style}'. Use one of: {}",
            STYLES.join(", ")
        ));
    }
    let default = if style == "mondrian" {
        "mondrian"
    } else {
        "viridis"
    };
    let palette = value("palette").map_or_else(|| default.into(), |it| it.to_string());
    if !PALETTES.iter().any(|(name, _)| *name == palette) {
        let names: Vec<&str> = PALETTES.iter().map(|(name, _)| *name).collect();
        return Err(format!(
            "Unknown arts.palette '{palette}'. Use one of: {}",
            names.join(", ")
        ));
    }
    let colors = match value("colors") {
        Some(Value::List(items)) => Some(
            items
                .borrow()
                .items
                .iter()
                .map(|it| color(&it.to_string()))
                .collect::<Result<_, _>>()?,
        ),
        Some(other) => Some(vec![color(&other.to_string())?]),
        None => None,
    };
    let background = value("background")
        .map(|it| color(&it.to_string()))
        .transpose()?;
    let (width, height) = (whole("width")?, whole("height")?);
    if !(16..=10_000).contains(&width) || !(16..=10_000).contains(&height) {
        return Err(format!(
            "arts.width and arts.height must be between 16 and 10000, got {width}x{height}"
        ));
    }
    let file = value("file").map(|it| it.to_string());
    if let Some(file) = &file
        && !(file.to_lowercase().ends_with(".svg") || file.to_lowercase().ends_with(".png"))
    {
        return Err(format!(
            "Unsupported arts.file extension '{file}'. Use .svg or .png"
        ));
    }
    if !["radial", "mondrian", "phyllotaxis", "ridgeline"].contains(&style.as_str()) {
        tracing::warn!("nf-arts: uwe does not draw the style '{style}' and draws 'radial' instead");
    }
    let settings = Settings {
        file,
        style,
        palette,
        colors,
        background,
        width: width as u32,
        height: height as u32,
        title: flag("title")?,
    };
    Ok(Some(Box::new(Arts {
        settings,
        start: None,
        marks: Vec::new(),
    })))
}

/// What the run told the plugin so far.
struct Arts {
    settings: Settings,
    start: Option<Box<Start>>,
    marks: Vec<Mark>,
}

impl Observer for Arts {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.start = Some(start.clone()),
            Event::TaskCompleted(task) | Event::TaskFailed(task) | Event::TaskCached(task) => {
                self.marks.push(mark(task))
            }
            Event::RunEnded(_) if !self.marks.is_empty() => {
                if let Err(error) = self.write() {
                    tracing::warn!("nf-arts: unable to create artwork -- {error}");
                }
            }
            _ => {}
        }
    }
}

fn mark(task: &Task) -> Mark {
    let number = |key: &str| task.record.get(key).and_then(serde_json::Value::as_f64);
    let status = task.record["status"].as_str().unwrap_or_default();
    Mark {
        process: task.record["process"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        start: number("start").or(number("submit")).unwrap_or_default(),
        duration: number("realtime")
            .or(number("duration"))
            .unwrap_or_default(),
        memory: number("peak_rss"),
        cached: status == "CACHED",
        failed: !matches!(status, "COMPLETED" | "CACHED"),
    }
}

impl Arts {
    fn write(&self) -> std::io::Result<()> {
        let Some(start) = &self.start else {
            return Ok(());
        };
        let settings = &self.settings;
        let mut processes: Vec<&str> = Vec::new();
        let mut marks: Vec<&Mark> = self.marks.iter().collect();
        marks.sort_by(|one, other| one.start.total_cmp(&other.start));
        for mark in marks {
            if !processes.contains(&mark.process.as_str()) {
                processes.push(&mark.process);
            }
        }
        let palette = (PALETTES.iter())
            .find(|(name, _)| *name == settings.palette)
            .map_or(PALETTES[0].1, |(_, colors)| colors);
        let parsed: Vec<Color> = palette.iter().filter_map(|it| color(it).ok()).collect();
        let (background, choices) = match &settings.colors {
            Some(colors) => (settings.background.unwrap_or([0xff; 3]), colors.clone()),
            None => (
                settings.background.unwrap_or(parsed[0]),
                parsed[1..].to_vec(),
            ),
        };
        let colors = match settings.palette == "mondrian" && settings.colors.is_none() {
            true => (0..processes.len())
                .map(|it| choices[it % choices.len()])
                .collect(),
            false => blend(&choices, processes.len()),
        };
        let caption_room = if settings.title { 0.9 } else { 1.0 };
        let scene = Scene {
            marks: &self.marks,
            processes,
            colors,
            width: f64::from(settings.width),
            height: f64::from(settings.height) * caption_room,
            seed: seed(&start.session_id),
        };
        let shapes = match settings.style.as_str() {
            "mondrian" => draw::mondrian(&scene),
            "phyllotaxis" => draw::phyllotaxis(&scene),
            "ridgeline" => draw::ridgeline(&scene),
            _ => draw::radial(&scene),
        };
        let luma = (0.299 * f64::from(background[0])
            + 0.587 * f64::from(background[1])
            + 0.114 * f64::from(background[2]))
            / 255.0;
        let picture = Picture {
            width: settings.width,
            height: settings.height,
            background,
            shapes,
            caption: match settings.title {
                true => vec![caption(start), "drawn by uwe after nf-arts 1.0.0".into()],
                false => Vec::new(),
            },
            ink: if luma > 0.5 { [0x22; 3] } else { [0xee; 3] },
        };
        let name = (settings.file.clone()).unwrap_or_else(|| format!("art-{}.svg", start.run_name));
        let file = Path::new(&start.launch_dir).join(&name);
        let bytes = match name.to_lowercase().ends_with(".png") {
            true => picture.png()?,
            false => picture.svg().into_bytes(),
        };
        if let Some(folder) = file.parent() {
            std::fs::create_dir_all(folder)?;
        }
        std::fs::write(&file, bytes)?;
        tracing::info!("nf-arts: artwork saved to {name}");
        Ok(())
    }
}

/// `<pipeline> · <version> · <run name>`, leaving out what the run does not have.
fn caption(start: &Start) -> String {
    let manifest = |key: &str| start.manifest[key].as_str().filter(|it| !it.is_empty());
    let pipeline =
        manifest("name").or(Some(start.project_name.as_str()).filter(|it| !it.is_empty()));
    let version = manifest("version").or(start.revision.as_deref());
    let parts = [pipeline, version, Some(start.run_name.as_str())];
    parts
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// A colour per process along the colours: the first gets the first, the last the last, those
/// between the blend at their place.
fn blend(colors: &[Color], count: usize) -> Vec<Color> {
    if colors.len() < 2 || count < 2 {
        return (0..count)
            .map(|it| colors[it.min(colors.len() - 1)])
            .collect();
    }
    (0..count)
        .map(|place| {
            let at = place as f64 / (count - 1) as f64 * (colors.len() - 1) as f64;
            let low = (at.floor() as usize).min(colors.len() - 2);
            let share = at - low as f64;
            let (one, other) = (colors[low], colors[low + 1]);
            [0, 1, 2].map(|channel| {
                let mixed =
                    f64::from(one[channel]) * (1.0 - share) + f64::from(other[channel]) * share;
                mixed.round() as u8
            })
        })
        .collect()
}

/// A colour of `#rrggbb` text.
fn color(text: &str) -> Result<Color, String> {
    let invalid = || format!("Invalid arts color '{text}'. Use #rrggbb");
    let digits = text
        .strip_prefix('#')
        .filter(|it| it.len() == 6)
        .ok_or_else(invalid)?;
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).map_err(|_| invalid());
    Ok([channel(0)?, channel(2)?, channel(4)?])
}

/// The seed of a run's choices: FNV-1a of its session id.
fn seed(session: &str) -> u64 {
    (session.bytes()).fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blends_palette() {
        let viridis: Vec<Color> = PALETTES[0].1[1..]
            .iter()
            .filter_map(|it| color(it).ok())
            .collect();
        // Two processes of the recorded picture: the first and the last colour.
        let two: Vec<String> = blend(&viridis, 2).into_iter().map(image::hex).collect();
        assert_eq!(two, ["#5b2a86", "#fde725"]);
        assert_eq!(
            blend(&viridis, 5)
                .iter()
                .map(|it| image::hex(*it))
                .nth(2)
                .as_deref(),
            Some("#21918c")
        );
        assert_eq!(
            color("#12345"),
            Err("Invalid arts color '#12345'. Use #rrggbb".into())
        );
    }
}
