//! The job service of an attempt: its name, its specification with the task container and the
//! helper container, and the `EXECUTE JOB SERVICE` statement that starts it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use rsa::sha2::{Digest, Sha256};
use serde_json::{Map, Value, json};
use task_model::Placement;

use crate::settings::Settings;

/// The container of the task, whose logs uwe reads.
pub const MAIN: &str = "main";

/// The words that the helper image runs to copy its uwe binary to the shared volume.
const INSTALL: [&str; 3] = ["/uwe", "__stage", "install"];

/// Waits until the helper container placed the binary `$1` and its size stays, then runs the
/// rest of the words; containers of a job service start together, without init containers.
const WAIT: &str = "tries=0\nwhile :; do\n    if [ -s \"$1\" ]; then\n        size=$(wc -c < \
                    \"$1\")\n        sleep 1\n        [ \"$size\" = \"$(wc -c < \"$1\")\" ] && \
                    break\n    fi\n    tries=$((tries + 1))\n    [ \"$tries\" -lt 600 ] || { echo \
                    \"uwe: the helper container placed no helper at $1\" >&2; exit 125; }\n    \
                    sleep 1\ndone\nshift\nexec \"$@\"\n";

/// What the job service of an attempt runs.
pub struct Shape<'a> {
    pub name: &'a str,
    pub attempt: &'a str,
    /// The image of the task, after the registry mappings.
    pub image: &'a str,
    /// The arguments of the task container, after the entrypoint of its image.
    pub command: Vec<String>,
    pub placement: &'a Placement,
    /// The helper image and the folder where both containers see the binary that it places.
    pub helper: Option<(&'a str, &'a str)>,
}

/// The name of the job service of an attempt: unique for the attempt and the moment.
pub fn job_name(attempt: &str, moment: u128) -> String {
    let digest = Sha256::digest(format!("{attempt}\n{moment}").as_bytes());
    let hex: String = digest
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02X}"))
        .collect();
    format!("UWE_{hex}")
}

/// A request id in the form of a UUID, which makes a retried submission run only once.
pub fn request_id(name: &str) -> String {
    let hex: String = (Sha256::digest(name.as_bytes()).iter())
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The image after the first mapping whose prefix is the first part of its name, as
/// nf-snowflake's `registryMappings` replace it.
pub fn mapped(image: &str, mappings: &[(String, String)]) -> String {
    let Some((first, rest)) = image.split_once('/') else {
        return image.to_owned();
    };
    match mappings.iter().find(|(from, _)| from == first) {
        Some((_, to)) => format!("{}/{rest}", to.trim_end_matches('/')),
        None => image.to_owned(),
    }
}

/// The specification of the job service, in JSON, which Snowflake reads as YAML.
pub fn spec(shape: &Shape<'_>) -> Value {
    let placement = shape.placement;
    let mut requests = Map::new();
    requests.insert("cpu".into(), json!(placement.cpus.max(1)));
    if let Some(memory) = placement.memory_bytes {
        requests.insert(
            "memory".into(),
            json!(format!("{}Mi", memory.div_ceil(1 << 20))),
        );
    }
    let mut resources = json!({"requests": requests});
    let gpus: u32 = placement.accelerators.iter().map(|gpu| gpu.count).sum();
    if gpus > 0 {
        resources["requests"]["nvidia.com/gpu"] = json!(gpus);
        resources["limits"] = json!({"nvidia.com/gpu": gpus});
    }
    let mut main = json!({
        "name": MAIN,
        "image": shape.image,
        "args": shape.command,
        "resources": resources,
    });
    let mut containers = Vec::new();
    let mut volumes = Vec::new();
    if let Some((image, folder)) = shape.helper {
        let mount = json!([{"name": "helper", "mountPath": folder}]);
        let binary = format!("{folder}/uwe");
        let mut command = vec![
            "sh".to_owned(),
            "-c".into(),
            WAIT.into(),
            "uwe".into(),
            binary.clone(),
        ];
        command.extend(shape.command.iter().cloned());
        main["args"] = json!(command);
        main["volumeMounts"] = mount.clone();
        let mut install: Vec<String> = INSTALL.map(str::to_owned).to_vec();
        install.push(binary);
        containers.push(main);
        containers.push(json!({
            "name": "helper",
            "image": image,
            "command": install,
            "volumeMounts": mount,
        }));
        volumes.push(json!({"name": "helper", "source": "local"}));
    } else {
        containers.push(main);
    }
    let mut spec = json!({"containers": containers});
    if !volumes.is_empty() {
        spec["volumes"] = json!(volumes);
    }
    json!({"spec": spec})
}

/// The `EXECUTE JOB SERVICE` statement of `shape` on the compute pool of `settings`.
pub fn statement(shape: &Shape<'_>, settings: &Settings) -> Result<String, String> {
    let pool = (settings.compute_pool.as_deref())
        .ok_or("the Snowflake executor needs a compute pool: set compute_pool")?;
    let spec = spec(shape).to_string();
    // The specification sits between `$$` marks, which it must not hold itself.
    if spec.contains("$$") {
        return Err("the job specification holds `$$`, which Snowflake cannot quote".into());
    }
    let comment = format!("uwe attempt: {}", shape.attempt).replace('\'', "''");
    let mut text = format!(
        "EXECUTE JOB SERVICE\nIN COMPUTE POOL {pool}\nNAME = {}\nCOMMENT = '{comment}'\n",
        shape.name
    );
    if !settings.integrations.is_empty() {
        let names = settings.integrations.join(", ");
        text.push_str(&format!("EXTERNAL_ACCESS_INTEGRATIONS = ({names})\n"));
    }
    text.push_str(&format!("FROM SPECIFICATION $$\n{spec}\n$$"));
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::identifier;
    use task_model::Accelerator;

    #[test]
    fn builds_job_services() {
        let placement = Placement {
            cpus: 2,
            memory_bytes: Some(3 << 30),
            accelerators: vec![Accelerator {
                count: 1,
                kind: None,
            }],
            ..Placement::default()
        };
        let shape = Shape {
            name: "UWE_0A",
            attempt: "run-it's-1",
            image: "/db/images/repo/ubuntu:24.04",
            command: vec!["sh".into(), "-c".into(), "true".into()],
            placement: &placement,
            helper: Some(("/db/images/repo/uwe:0.1.0", "/tmp/uwe/a.helper")),
        };
        let spec = spec(&shape);
        let main = &spec["spec"]["containers"][0];
        assert_eq!(main["name"], MAIN);
        assert_eq!(main["resources"]["requests"]["cpu"], 2);
        assert_eq!(main["resources"]["requests"]["memory"], "3072Mi");
        assert_eq!(main["resources"]["limits"]["nvidia.com/gpu"], 1);
        assert!(
            main.get("command").is_none(),
            "the entrypoint of the image stays"
        );
        assert_eq!(main["args"][4], "/tmp/uwe/a.helper/uwe");
        assert_eq!(main["args"][7], "true");
        let helper = &spec["spec"]["containers"][1];
        let install = json!(["/uwe", "__stage", "install", "/tmp/uwe/a.helper/uwe"]);
        assert_eq!(helper["command"], install);
        assert_eq!(spec["spec"]["volumes"][0]["source"], "local");
        let settings = Settings {
            compute_pool: Some("TASKS".into()),
            integrations: vec!["STAGING".into()],
            ..Settings::default()
        };
        let text = statement(&shape, &settings).expect("a statement");
        assert!(text.starts_with("EXECUTE JOB SERVICE\nIN COMPUTE POOL TASKS\nNAME = UWE_0A\n"));
        assert!(
            text.contains("COMMENT = 'uwe attempt: run-it''s-1'"),
            "{text}"
        );
        assert!(
            text.contains("EXTERNAL_ACCESS_INTEGRATIONS = (STAGING)\n"),
            "{text}"
        );
        assert!(text.ends_with("}}\n$$"), "{text}");
        assert!(
            statement(&shape, &Settings::default()).is_err(),
            "no compute pool"
        );
    }

    #[test]
    fn names_snowflake_objects() {
        let mappings = [("docker.io".to_owned(), "/db/images/repo".to_owned())];
        let image = mapped("docker.io/library/ubuntu:24.04", &mappings);
        assert_eq!(image, "/db/images/repo/library/ubuntu:24.04");
        assert_eq!(mapped("quay.io/x:1", &mappings), "quay.io/x:1");
        assert_eq!(mapped("ubuntu", &mappings), "ubuntu");
        let name = job_name("run-A-1-1", 1);
        assert!(identifier(&name) && name.len() == 28, "{name}");
        assert_eq!(request_id("UWE_0A").len(), 36);
    }
}
