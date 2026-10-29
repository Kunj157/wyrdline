//! Parquet projections of bundles and the queries over them, with an
//! embedded DuckDB (feature `analytics`).
//!
//! Each verified bundle gets `projections/<org>/<bundle>/{events,
//! divergences,guard,judge}.parquet` under the data directory, written
//! on upload. A missing projection (another replica, a wiped volume) is
//! rebuilt from the stored bundle on first use. Every value that comes
//! from a user reaches DuckDB as a bound parameter; the only text spliced
//! into SQL is the server's own file paths, quoted.

use std::path::{Path, PathBuf};

use duckdb::{Connection, params, params_from_iter};

use crate::projection::{JudgeStats, Query, Rows, Stats};

pub const FILES: [&str; 4] = ["events", "divergences", "guard", "judge"];

/// The projection directory of one bundle.
pub fn dir(data_dir: &Path, org_id: &str, bundle_id: &str) -> PathBuf {
    data_dir.join("projections").join(org_id).join(bundle_id)
}

/// A complete projection exists (directories are renamed into place).
pub fn exists(dir: &Path) -> bool {
    FILES
        .iter()
        .all(|f| dir.join(format!("{f}.parquet")).is_file())
}

/// `'…'` with quotes doubled: for the server's own paths only.
fn quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

/// Write the projection of `rows` to `dir` atomically (blocking).
pub fn write(rows: &Rows, dir: &Path) -> Result<(), duckdb::Error> {
    let parent = dir.parent().unwrap_or(dir);
    std::fs::create_dir_all(parent).map_err(io)?;
    let tmp = parent.join(format!(".tmp-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&tmp).map_err(io)?;
    let result = (|| {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE events (seq BIGINT, plane VARCHAR, kind VARCHAR, ts_wall_ms BIGINT, \
               pid BIGINT, path VARCHAR, target VARCHAR, detail VARCHAR, outcome VARCHAR, drops BIGINT);
             CREATE TABLE divergences (seq BIGINT, rule_id VARCHAR, severity VARCHAR, \
               confidence_bp INTEGER, summary VARCHAR);
             CREATE TABLE guard (seq BIGINT, stage VARCHAR, decision VARCHAR, policy_id VARCHAR, \
               reason VARCHAR);
             CREATE TABLE judge (model VARCHAR, prompt_version VARCHAR, input_tokens BIGINT, \
               output_tokens BIGINT, cache_creation_input_tokens BIGINT, \
               cache_read_input_tokens BIGINT, cost_micro_usd BIGINT, latency_ms BIGINT, \
               failed BOOLEAN);",
        )?;
        {
            let mut a = db.appender("events")?;
            for r in &rows.events {
                a.append_row(params![
                    r.seq,
                    r.plane,
                    r.kind,
                    r.ts_wall_ms,
                    r.pid,
                    r.path,
                    r.target,
                    r.detail,
                    r.outcome,
                    r.drops
                ])?;
            }
            let mut a = db.appender("divergences")?;
            for r in &rows.divergences {
                a.append_row(params![
                    r.seq,
                    r.rule_id,
                    r.severity,
                    r.confidence_bp,
                    r.summary
                ])?;
            }
            let mut a = db.appender("guard")?;
            for r in &rows.guard {
                a.append_row(params![r.seq, r.stage, r.decision, r.policy_id, r.reason])?;
            }
            let mut a = db.appender("judge")?;
            for r in &rows.judge {
                a.append_row(params![
                    r.model,
                    r.prompt_version,
                    r.input_tokens,
                    r.output_tokens,
                    r.cache_creation_input_tokens,
                    r.cache_read_input_tokens,
                    r.cost_micro_usd,
                    r.latency_ms,
                    r.failed
                ])?;
            }
        }
        for f in FILES {
            db.execute_batch(&format!(
                "COPY {f} TO {} (FORMAT parquet)",
                quote(&tmp.join(format!("{f}.parquet")))
            ))?;
        }
        Ok(())
    })();
    match result.and_then(|()| std::fs::rename(&tmp, dir).map_err(io)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            // Another writer won the race: its projection is as good.
            if exists(dir) { Ok(()) } else { Err(e) }
        }
    }
}

fn io(e: std::io::Error) -> duckdb::Error {
    duckdb::Error::ToSqlConversionFailure(Box::new(e))
}

/// `read_parquet([...], filename = true)` over `file` of every dir.
fn source(dirs: &[PathBuf], file: &str) -> String {
    let list: Vec<String> = dirs
        .iter()
        .map(|d| quote(&d.join(format!("{file}.parquet"))))
        .collect();
    format!("read_parquet([{}], filename = true)", list.join(", "))
}

/// The bundle id from a projection file name.
const BUNDLE_OF: &str = "regexp_extract(filename, '([0-9A-Z]{26})[/\\\\][a-z]+\\.parquet$', 1)";

/// Aggregate the projections in `dirs` (blocking).
pub fn stats(dirs: &[PathBuf]) -> Result<Stats, duckdb::Error> {
    if dirs.is_empty() {
        return Ok(Stats::default());
    }
    let db = Connection::open_in_memory()?;
    let mut out = Stats::default();
    let mut q = db.prepare(&format!(
        "SELECT rule_id, count(DISTINCT {BUNDLE_OF}), count(*) FROM {} \
         GROUP BY rule_id ORDER BY rule_id",
        source(dirs, "divergences")
    ))?;
    out.rules = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let events = source(dirs, "events");
    let mut q = db.prepare(&format!(
        "SELECT kind, count(*) FROM {events} WHERE outcome = 'denied' GROUP BY kind ORDER BY kind"
    ))?;
    out.denied = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut q = db.prepare(&format!(
        "SELECT count(*), coalesce(sum(drops), 0)::BIGINT, \
         count(*) FILTER (WHERE kind = 'meta.sensor.lost') FROM {events}"
    ))?;
    (out.events, out.dropped, out.sensor_lost) =
        q.query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    out.judge = judge(&db, dirs)?;
    Ok(out)
}

fn judge(db: &Connection, dirs: &[PathBuf]) -> Result<JudgeStats, duckdb::Error> {
    let judge = source(dirs, "judge");
    let mut out = JudgeStats::default();
    let mut q = db.prepare(&format!(
        "SELECT count(*), count(*) FILTER (WHERE failed), \
         coalesce(sum(input_tokens), 0)::BIGINT, coalesce(sum(output_tokens), 0)::BIGINT, \
         coalesce(sum(cache_creation_input_tokens), 0)::BIGINT, \
         coalesce(sum(cache_read_input_tokens), 0)::BIGINT, \
         coalesce(sum(cost_micro_usd), 0)::BIGINT FROM {judge}"
    ))?;
    (
        out.runs,
        out.failed,
        out.input_tokens,
        out.output_tokens,
        out.cache_creation_input_tokens,
        out.cache_read_input_tokens,
        out.cost_micro_usd,
    ) = q.query_row([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })?;
    let mut q = db.prepare(&format!(
        "SELECT model, count(*), coalesce(sum(cost_micro_usd), 0)::BIGINT FROM {judge} \
         GROUP BY model ORDER BY count(*) DESC, model"
    ))?;
    out.by_model = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(out)
}

/// One matching event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub bundle_id: String,
    pub seq: i64,
    pub kind: String,
    pub path: Option<String>,
    pub target: Option<String>,
    pub detail: Option<String>,
}

/// Events in `dirs` matching the event filters of `q` (blocking).
pub fn search(dirs: &[PathBuf], q: &Query, limit: i64) -> Result<Vec<Hit>, duckdb::Error> {
    if dirs.is_empty() {
        return Ok(Vec::new());
    }
    let mut conds: Vec<&str> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    for p in &q.paths {
        conds.push("contains(coalesce(path, ''), ?)");
        args.push(p.clone());
    }
    for k in &q.kinds {
        conds.push("(kind = ? OR starts_with(kind, ? || '.'))");
        args.extend([k.clone(), k.clone()]);
    }
    for h in &q.hosts {
        conds.push(
            "(starts_with(kind, 'effect.net.') AND contains(lower(coalesce(target, '')), lower(?)))",
        );
        args.push(h.clone());
    }
    for e in &q.exes {
        conds.push("(kind = 'effect.proc.exec' AND contains(coalesce(target, ''), ?))");
        args.push(e.clone());
    }
    for t in &q.tools {
        conds.push("(kind = 'intent.tool_call.started' AND target = ?)");
        args.push(t.clone());
    }
    for w in &q.text {
        conds.push("contains(lower(concat_ws(' ', path, target, detail)), lower(?))");
        args.push(w.clone());
    }
    let filter = if conds.is_empty() {
        "TRUE".to_string()
    } else {
        conds.join(" AND ")
    };
    let db = Connection::open_in_memory()?;
    let mut stmt = db.prepare(&format!(
        "SELECT {BUNDLE_OF} AS bundle, seq, kind, path, target, detail FROM {} \
         WHERE {filter} ORDER BY bundle DESC, seq LIMIT {}",
        source(dirs, "events"),
        limit.clamp(1, 1000)
    ))?;
    let hits = stmt
        .query_map(params_from_iter(args), |r| {
            Ok(Hit {
                bundle_id: r.get(0)?,
                seq: r.get(1)?,
                kind: r.get(2)?,
                path: r.get(3)?,
                target: r.get(4)?,
                detail: r.get(5)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(hits)
}
