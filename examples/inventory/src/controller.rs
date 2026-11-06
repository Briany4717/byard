//! The one thing Stockroom needs that byld cannot do: write a file.
//! `exportCsv` writes the catalogue to the app's data directory and says where.

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
}

impl Stockroom {
    /// The controller for the app named `app_name`, writing under the same
    /// data directory `Store` keeps its file in.
    #[must_use]
    pub fn for_app(app_name: &str) -> Self {
        Self::at(byard::data_dir(app_name))
    }

    /// The controller writing under `dir`.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

#[byard::byard_controller]
impl Stockroom {
    /// Writes `rows` as CSV into the data directory. Answers with the file's
    /// `path` and how many `rows` it holds.
    // The controller macro converts only owned arguments, so this takes the Vec.
    #[allow(clippy::needless_pass_by_value)]
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

/// Where the export of a controller writing under `dir` ends up.
#[must_use]
pub fn export_path(dir: &Path) -> PathBuf {
    dir.join(EXPORT_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

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
