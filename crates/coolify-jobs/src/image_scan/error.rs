//! `ImageScanErrorCode`, `ScanFailed`, `ScanLockBusy`, `ImageScanningRefused` and
//! `ImageUpdateRefused`.

use serde_json::{Map, Value};

/// `App\Enums\ImageScanErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    DiskSpace,
    DbTooOld,
    DbMissing,
    Timeout,
    OutOfMemory,
    LockBusy,
    RegistryAuth,
    BuildServerOff,
    ScanningOff,
    CustomCommand,
    ImageMissing,
    PullFailed,
    Truncated,
    NoPackageDatabase,
    Unknown,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 15] = [
        ErrorCode::DiskSpace,
        ErrorCode::DbTooOld,
        ErrorCode::DbMissing,
        ErrorCode::Timeout,
        ErrorCode::OutOfMemory,
        ErrorCode::LockBusy,
        ErrorCode::RegistryAuth,
        ErrorCode::BuildServerOff,
        ErrorCode::ScanningOff,
        ErrorCode::CustomCommand,
        ErrorCode::ImageMissing,
        ErrorCode::PullFailed,
        ErrorCode::Truncated,
        ErrorCode::NoPackageDatabase,
        ErrorCode::Unknown,
    ];

    /// The stored value (`$code->value`).
    pub fn value(self) -> &'static str {
        match self {
            ErrorCode::DiskSpace => "disk_space",
            ErrorCode::DbTooOld => "db_too_old",
            ErrorCode::DbMissing => "db_missing",
            ErrorCode::Timeout => "timeout",
            ErrorCode::OutOfMemory => "out_of_memory",
            ErrorCode::LockBusy => "lock_busy",
            ErrorCode::RegistryAuth => "registry_auth",
            ErrorCode::BuildServerOff => "build_server_off",
            ErrorCode::ScanningOff => "scanning_off",
            ErrorCode::CustomCommand => "custom_command",
            ErrorCode::ImageMissing => "image_missing",
            ErrorCode::PullFailed => "pull_failed",
            ErrorCode::Truncated => "truncated",
            ErrorCode::NoPackageDatabase => "no_package_database",
            ErrorCode::Unknown => "unknown",
        }
    }

    /// `ImageScanErrorCode::tryFrom($value)`.
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.value() == value)
    }

    /// `ImageScanErrorCode::message($context)`: the fixed user-facing message. Raw command output
    /// is never shown. The context keys are `server`, `n` and `step`.
    pub fn message(self, context: &Map<String, Value>) -> String {
        let text = |key: &str| match context.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let server = text("server").unwrap_or_else(|| "the server".to_owned());
        let n = text("n").unwrap_or_else(|| "?".to_owned());
        match self {
            ErrorCode::DiskSpace => {
                if context.get("n").is_some_and(|v| !v.is_null()) {
                    format!("Not enough disk space on {server}: {n} GB needed.")
                } else {
                    format!("Not enough disk space on {server}.")
                }
            }
            ErrorCode::DbTooOld => format!(
                "The vulnerability database is {n} days old. {server} cannot reach grype.anchore.io."
            ),
            ErrorCode::DbMissing => format!(
                "The vulnerability database is not on {server} yet. Wait for the first update to finish or check that {server} can reach grype.anchore.io."
            ),
            ErrorCode::Timeout => match context.get("step").and_then(Value::as_str) {
                Some("sbom") => format!(
                    "Syft took too long and was stopped. Raise the Syft time limit on the Vulnerabilities page of {server}."
                ),
                Some("grype") => format!(
                    "Grype took too long and was stopped. Raise the Grype time limit on the Vulnerabilities page of {server}."
                ),
                _ => "The scan took too long and was stopped.".to_owned(),
            },
            ErrorCode::OutOfMemory => format!(
                "The scanner ran out of memory and was stopped. Raise the scanner memory limit on the Vulnerabilities page of {server}, and make sure that {server} has that much free memory."
            ),
            ErrorCode::LockBusy => format!("Another scan is still running on {server}."),
            ErrorCode::RegistryAuth => format!(
                "Coolify cannot read this image from its registry. Log in to the registry on {server}."
            ),
            ErrorCode::BuildServerOff => "Image scanning is off on the build server.".to_owned(),
            ErrorCode::ScanningOff => format!("Turn on image scanning on {server}."),
            ErrorCode::CustomCommand => "Coolify cannot check the image because this application uses a custom build or start command.".to_owned(),
            ErrorCode::ImageMissing => format!(
                "The image is not on {server}. Click Scan now to pull and scan it. An image that Coolify builds needs a new deploy."
            ),
            ErrorCode::PullFailed => format!(
                "Coolify could not pull the image to {server}. Check the image name, and log in to its registry on {server} if it is private."
            ),
            ErrorCode::Truncated => "The scan result was too large to read completely.".to_owned(),
            ErrorCode::NoPackageDatabase => format!(
                "Coolify found no package database of a supported package manager (dpkg, rpm, apk, pacman) on {server}."
            ),
            ErrorCode::Unknown => "The scan could not run.".to_owned(),
        }
    }
}

/// `mb_substr($text, 0, $max)`.
pub(crate) fn mb_prefix(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// `ScanFailed`: stops a scan with an error code. `ScanImage` catches it and stores the code on
/// the scan row. The detail is for the log only: users see the fixed message of the error code.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanFailed {
    pub code: ErrorCode,
    /// The values of the error message (`n`, `step`).
    pub context: Map<String, Value>,
    pub detail: Option<String>,
}

impl ScanFailed {
    const DETAIL_LIMIT: usize = 1000;

    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            context: Map::new(),
            detail: None,
        }
    }

    /// `new ScanFailed($code, detail: $detail)`.
    pub fn detail(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            context: Map::new(),
            detail: Some(detail.into()),
        }
    }

    /// `new ScanFailed($code, $context, $detail)`.
    pub fn with(code: ErrorCode, context: Map<String, Value>, detail: Option<String>) -> Self {
        Self {
            code,
            context,
            detail,
        }
    }

    /// `getMessage()`: the code, then the detail cut to 1000 characters.
    pub fn message(&self) -> String {
        match &self.detail {
            None => self.code.value().to_owned(),
            Some(detail) => format!(
                "{}: {}",
                self.code.value(),
                mb_prefix(detail, Self::DETAIL_LIMIT)
            ),
        }
    }

    /// `log($server, $target)`: writes the failure with its detail to the log, so an admin can
    /// find why a scan could not run.
    pub fn log(&self, server_name: &str, server_uuid: &str, target: &str) {
        let context = Value::Object(self.context.clone());
        tracing::warn!(
            server_uuid,
            error_code = self.code.value(),
            context = %context,
            "Image scan of {target} on server {server_name} failed: {}",
            self.message()
        );
    }

    /// `errorContext($context)`: the values of the error message, plus the detail for the error
    /// log on the UI (`detail`; the message ignores it).
    pub fn error_context(&self, context: &Map<String, Value>) -> Map<String, Value> {
        let mut out = context.clone();
        for (k, v) in &self.context {
            out.insert(k.clone(), v.clone());
        }
        if let Some(detail) = &self.detail {
            out.insert(
                "detail".to_owned(),
                Value::String(mb_prefix(detail, Self::DETAIL_LIMIT)),
            );
        }
        out
    }

    /// `outputTail($output)`: the last part of the raw command output, for the detail.
    pub fn output_tail(output: Option<&str>) -> String {
        let output = coolify_remote::text::php_trim(output.unwrap_or_default());
        if output.is_empty() {
            return "(no output)".to_owned();
        }
        let count = output.chars().count();
        output
            .chars()
            .skip(count.saturating_sub(Self::DETAIL_LIMIT / 2))
            .collect()
    }
}

impl std::fmt::Display for ScanFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ScanFailed {}

/// `ScanLockBusy`: a background candidate check found the server lock busy (or a deploy gate
/// waiting for it) and stopped without storing a result, so its job can retry later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanLockBusy;

impl std::fmt::Display for ScanLockBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The image scan lock of the server is busy.")
    }
}

impl std::error::Error for ScanLockBusy {}

/// `ImageScanningRefused`: `SetServerImageScanning` did not turn scanning on. The message is
/// shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageScanningRefused(pub String);

impl std::fmt::Display for ImageScanningRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ImageScanningRefused {}

/// `ImageUpdateRefused`: `UpdateResourceImage` did not change anything. The message is shown to
/// the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageUpdateRefused(pub String);

impl std::fmt::Display for ImageUpdateRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ImageUpdateRefused {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ctx(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn values_round_trip() {
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_value(code.value()), Some(code));
        }
        assert_eq!(ErrorCode::from_value("nope"), None);
    }

    #[test]
    fn messages_use_the_context() {
        assert_eq!(
            ErrorCode::DiskSpace.message(&ctx(json!({"server": "web-1", "n": 3}))),
            "Not enough disk space on web-1: 3 GB needed."
        );
        assert_eq!(
            ErrorCode::DiskSpace.message(&Map::new()),
            "Not enough disk space on the server."
        );
        assert_eq!(
            ErrorCode::DbTooOld.message(&ctx(json!({"server": "web-1"}))),
            "The vulnerability database is ? days old. web-1 cannot reach grype.anchore.io."
        );
        assert_eq!(
            ErrorCode::Timeout.message(&ctx(json!({"server": "web-1", "step": "sbom"}))),
            "Syft took too long and was stopped. Raise the Syft time limit on the Vulnerabilities page of web-1."
        );
        assert_eq!(
            ErrorCode::Timeout.message(&ctx(json!({"step": "save"}))),
            "The scan took too long and was stopped."
        );
        assert_eq!(
            ErrorCode::ScanningOff.message(&ctx(json!({"server": "web-1"}))),
            "Turn on image scanning on web-1."
        );
        assert_eq!(
            ErrorCode::Unknown.message(&Map::new()),
            "The scan could not run."
        );
    }

    #[test]
    fn scan_failed_message_and_context() {
        let plain = ScanFailed::new(ErrorCode::ImageMissing);
        assert_eq!(plain.message(), "image_missing");
        assert_eq!(
            plain.error_context(&ctx(json!({"server": "web-1"}))),
            ctx(json!({"server": "web-1"}))
        );

        let failed = ScanFailed::with(
            ErrorCode::Timeout,
            ctx(json!({"step": "grype"})),
            Some("step grype exited with code 124 (timeout)".into()),
        );
        assert_eq!(
            failed.message(),
            "timeout: step grype exited with code 124 (timeout)"
        );
        assert_eq!(
            failed.error_context(&ctx(json!({"server": "web-1"}))),
            ctx(
                json!({"server": "web-1", "step": "grype", "detail": "step grype exited with code 124 (timeout)"})
            )
        );

        let long = ScanFailed::detail(ErrorCode::Unknown, "x".repeat(1500));
        assert_eq!(long.message().len(), "unknown: ".len() + 1000);
    }

    #[test]
    fn output_tail_keeps_the_end() {
        assert_eq!(ScanFailed::output_tail(None), "(no output)");
        assert_eq!(ScanFailed::output_tail(Some("  \n")), "(no output)");
        assert_eq!(ScanFailed::output_tail(Some(" boom \n")), "boom");
        let long = format!("{}{}", "a".repeat(600), "b".repeat(500));
        assert_eq!(ScanFailed::output_tail(Some(&long)), "b".repeat(500));
    }
}
