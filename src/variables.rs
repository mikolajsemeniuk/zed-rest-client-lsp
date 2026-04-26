use std::collections::HashMap;

use chrono::{Duration, Utc};
use rand::Rng;
use regex::Regex;
use uuid::Uuid;

pub fn parse_file_variables(text: &str) -> HashMap<String, String> {
    let re = Regex::new(r"^\s*@(\w+)\s*=\s*(.+?)\s*$").unwrap();
    let mut vars = HashMap::new();

    for line in text.lines() {
        if let Some(caps) = re.captures(line) {
            let name = caps[1].to_string();
            let raw_value = caps[2].to_string();
            let resolved = substitute_user_vars(&raw_value, &vars);
            vars.insert(name, resolved);
        }
    }
    vars
}

fn substitute_user_vars(input: &str, vars: &HashMap<String, String>) -> String {
    let re = Regex::new(r"\{\{\s*([^$\s\}][^}]*?)\s*\}\}").unwrap();
    re.replace_all(input, |caps: &regex::Captures| {
        let name = caps[1].trim();
        vars.get(name)
            .cloned()
            .unwrap_or_else(|| caps[0].to_string())
    })
    .to_string()
}

fn substitute_system_vars(input: &str) -> Result<String, String> {
    let re = Regex::new(r"\{\{\s*\$(\w+)([^}]*?)\s*\}\}").unwrap();
    let mut last_err: Option<String> = None;

    let result = re.replace_all(input, |caps: &regex::Captures| {
        let name = &caps[1];
        let args = caps[2].trim();
        match resolve_system_var(name, args) {
            Ok(v) => v,
            Err(e) => {
                last_err = Some(e);
                caps[0].to_string()
            }
        }
    });

    if let Some(e) = last_err {
        Err(e)
    } else {
        Ok(result.to_string())
    }
}

fn resolve_system_var(name: &str, args: &str) -> Result<String, String> {
    match name {
        "guid" => Ok(Uuid::new_v4().to_string()),

        "timestamp" => {
            let now = Utc::now();
            if args.is_empty() {
                Ok(now.timestamp().to_string())
            } else {
                let dt = apply_offset(now, args)?;
                Ok(dt.timestamp().to_string())
            }
        }

        "datetime" => {
            let now = Utc::now();
            let (format_spec, offset_args) = split_datetime_args(args);
            let dt = if offset_args.is_empty() {
                now
            } else {
                apply_offset(now, &offset_args)?
            };
            format_datetime(&dt, &format_spec)
        }

        "randomInt" => {
            let parts: Vec<&str> = args.split_whitespace().collect();
            if parts.len() != 2 {
                return Err(format!(
                    "$randomInt wymaga 2 argumentów (min max), dostałem: '{args}'"
                ));
            }
            let min: i64 = parts[0]
                .parse()
                .map_err(|_| format!("$randomInt: '{}' nie jest liczbą", parts[0]))?;
            let max: i64 = parts[1]
                .parse()
                .map_err(|_| format!("$randomInt: '{}' nie jest liczbą", parts[1]))?;
            if min >= max {
                return Err(format!("$randomInt: min ({min}) musi być < max ({max})"));
            }
            let mut rng = rand::thread_rng();
            Ok(rng.gen_range(min..max).to_string())
        }

        "processEnv" => {
            let var_name = args.trim();
            if var_name.is_empty() {
                return Err("$processEnv wymaga nazwy zmiennej".to_string());
            }
            std::env::var(var_name)
                .map_err(|_| format!("$processEnv: zmienna '{var_name}' nie jest ustawiona"))
        }

        other => Err(format!("nieznana system variable: ${other}")),
    }
}

fn apply_offset(base: chrono::DateTime<Utc>, args: &str) -> Result<chrono::DateTime<Utc>, String> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        return Err(format!("offset wymaga 'N unit', dostałem: '{args}'"));
    }
    let n: i64 = parts[0]
        .parse()
        .map_err(|_| format!("offset: '{}' nie jest liczbą", parts[0]))?;
    let unit = parts[1];
    let duration = match unit {
        "s" => Duration::seconds(n),
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        "w" => Duration::weeks(n),
        "y" => Duration::days(n * 365),
        other => {
            return Err(format!(
                "nieznana jednostka offsetu: '{other}' (oczekiwane: s/m/h/d/w/y)"
            ))
        }
    };
    Ok(base + duration)
}

fn split_datetime_args(args: &str) -> (String, String) {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return ("iso8601".to_string(), String::new());
    }

    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
        let quote = trimmed.chars().next().unwrap();
        if let Some(end) = trimmed[1..].find(quote) {
            let format_only = &trimmed[1..=end];
            let rest = trimmed[end + 2..].trim().to_string();
            return (format_only.to_string(), rest);
        }

        return (trimmed.to_string(), String::new());
    }

    let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
    if parts.len() == 1 {
        (parts[0].to_string(), String::new())
    } else {
        (parts[0].to_string(), parts[1].trim().to_string())
    }
}

fn format_datetime(dt: &chrono::DateTime<Utc>, format_spec: &str) -> Result<String, String> {
    match format_spec {
        "rfc1123" => Ok(dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()),
        "iso8601" => Ok(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        custom => Ok(dt.format(custom).to_string()),
    }
}

pub fn substitute_all(input: &str, file_vars: &HashMap<String, String>) -> Result<String, String> {
    let after_user = substitute_user_vars(input, file_vars);

    let unresolved_re = Regex::new(r"\{\{\s*([^$\s\}][^}]*?)\s*\}\}").unwrap();
    if let Some(caps) = unresolved_re.captures(&after_user) {
        return Err(format!("nieznana zmienna: {{{{{}}}}}", caps[1].trim()));
    }

    substitute_system_vars(&after_user)
}
