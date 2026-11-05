//! The two things Stockroom needs that byld cannot do: read the clock and
//! write a file.
//!
//! `Stockroom.now()` is the time a movement is stamped with. `exportCsv` writes
//! the catalogue to the app's data directory and says where.

use std::path::{Path, PathBuf};

use byard::bridge::HostValue;

/// A product as the export reads it: the fields of the byld `Product`.
#[derive(Debug, Clone, PartialEq, byard::FromHostValue, byard::IntoHostValue)]
struct Row {
    id: i64,
    sku: String,
    name: String,
    category: String,
    qty: i64,
    min: i64,
    price: f64,
    location: Option<String>,
    updated: String,
}

/// The file the export writes, inside the data directory.
pub const EXPORT_FILE: &str = "stockroom-products.csv";

/// The controller views reach with `inject Stockroom as room`.
#[byard::byard_controller]
#[derive(Clone)]
pub struct Stockroom {
    /// Where files are written.
    dir: PathBuf,
    /// A clock that always says this, when the app is under test.
    fixed_now: Option<String>,
}

impl Stockroom {
    /// The controller for the app named `app_name`, writing under the same
    /// data directory `Store` keeps its file in.
    #[must_use]
    pub fn for_app(app_name: &str) -> Self {
        let dir = directories::ProjectDirs::from("dev", "byard", app_name).map_or_else(
            || PathBuf::from(".byard").join(app_name),
            |dirs| dirs.data_dir().to_path_buf(),
        );
        Self::at(dir)
    }

    /// The controller writing under `dir`.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            fixed_now: None,
        }
    }

    /// The same controller, whose clock always reads `now`.
    #[must_use]
    pub fn with_fixed_now(mut self, now: &str) -> Self {
        self.fixed_now = Some(now.to_string());
        self
    }
}

#[byard::byard_controller]
impl Stockroom {
    /// The time now, in UTC, as `2026-11-05T14:03`.
    fn now(&self) -> String {
        self.fixed_now.clone().unwrap_or_else(utc_now)
    }

    /// Writes `rows` as CSV into the data directory. Answers with the file's
    /// `path` and how many `rows` it holds.
    fn export_csv(&self, rows: Vec<Row>) -> Result<HostValue, HostValue> {
        let fail = |what: &str, e: &std::io::Error| {
            HostValue::Record(vec![
                ("kind".into(), HostValue::Str("io".into())),
                ("message".into(), HostValue::Str(format!("{what}: {e}"))),
            ])
        };
        std::fs::create_dir_all(&self.dir).map_err(|e| fail("cannot create the folder", &e))?;
        let path = self.dir.join(EXPORT_FILE);
        std::fs::write(&path, to_csv(&rows)).map_err(|e| fail("cannot write the file", &e))?;
        Ok(HostValue::Record(vec![
            (
                "path".into(),
                HostValue::Str(path.to_string_lossy().into_owned()),
            ),
            (
                "rows".into(),
                HostValue::Int(i64::try_from(rows.len()).unwrap_or(i64::MAX)),
            ),
        ]))
    }
}

/// The CSV text of `rows`, header first.
fn to_csv(rows: &[Row]) -> String {
    let mut out = String::from("id,sku,name,category,qty,min,price,location,updated\n");
    for r in rows {
        let line = [
            r.id.to_string(),
            field(&r.sku),
            field(&r.name),
            field(&r.category),
            r.qty.to_string(),
            r.min.to_string(),
            format!("{:.2}", r.price),
            field(r.location.as_deref().unwrap_or("")),
            field(&r.updated),
        ];
        out.push_str(&line.join(","));
        out.push('\n');
    }
    out
}

/// One CSV field, quoted when it holds a comma, a quote or a line break.
fn field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// The current UTC time as `YYYY-MM-DDTHH:MM`.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// Year, month and day of a count of days since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Where the export of a controller writing under `dir` ends up.
#[must_use]
pub fn export_path(dir: &Path) -> PathBuf {
    dir.join(EXPORT_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_become_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_427), (2025, 12, 31));
        assert_eq!(civil_from_days(19_782), (2024, 3, 6));
    }

    #[test]
    fn csv_quotes_what_needs_it() {
        let row = Row {
            id: 1,
            sku: "A-1".into(),
            name: "Bolt, M6 \"zinc\"".into(),
            category: "Hardware".into(),
            qty: 3,
            min: 1,
            price: 0.5,
            location: None,
            updated: "2026-01-01T00:00".into(),
        };
        let csv = to_csv(&[row]);
        assert_eq!(
            csv.lines().nth(1).unwrap(),
            "1,A-1,\"Bolt, M6 \"\"zinc\"\"\",Hardware,3,1,0.50,,2026-01-01T00:00"
        );
    }
}
