//! Codegen CLI: registry document in, typed modules out.
//!
//! ```text
//! cargo run -p cleverhans-codegen -- --schema registry.json --ts out.ts --py out.py --rs out.rs
//! ```
//!
//! With no output flag the TypeScript module goes to stdout. `--check`
//! writes nothing and fails if any named output is stale — CI's freshness
//! gate.

use std::process::ExitCode;

use cleverhans_codegen::{python_module, rust_module, typescript_module};
use cleverhans_core::schema::RegistrySchema;

struct Args {
    schema: String,
    ts: Option<String>,
    py: Option<String>,
    rs: Option<String>,
    check: bool,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut schema, mut ts, mut py, mut rs) = (None, None, None, None);
    let mut check = false;
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--schema" => &mut schema,
            "--ts" => &mut ts,
            "--py" => &mut py,
            "--rs" => &mut rs,
            "--check" => {
                check = true;
                continue;
            }
            other => return Err(format!("unknown flag `{other}`")),
        };
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if slot.replace(value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    Ok(Args {
        schema: schema.ok_or("--schema <file> is required")?,
        ts,
        py,
        rs,
        check,
    })
}

fn emit(path: &str, module: &str, check: bool) -> Result<(), String> {
    if check {
        let current = std::fs::read_to_string(path).unwrap_or_default();
        if current != module {
            return Err(format!("{path} is stale — re-run without --check"));
        }
        return Ok(());
    }
    std::fs::write(path, module).map_err(|err| format!("write {path}: {err}"))
}

fn run(args: Args) -> Result<(), String> {
    let json = std::fs::read_to_string(&args.schema)
        .map_err(|err| format!("read {}: {err}", args.schema))?;
    let schema = RegistrySchema::from_json(&json).map_err(|err| err.to_string())?;

    let ts_module = typescript_module(&schema.actions, &schema.blocks);
    match &args.ts {
        Some(path) => emit(path, &ts_module, args.check)?,
        None if args.py.is_none() && args.rs.is_none() && !args.check => print!("{ts_module}"),
        None => {}
    }
    if let Some(path) = &args.py {
        emit(
            path,
            &python_module(&schema.actions, &schema.blocks),
            args.check,
        )?;
    }
    if let Some(path) = &args.rs {
        emit(
            path,
            &rust_module(&schema.actions, &schema.blocks),
            args.check,
        )?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match parse_args(std::env::args().skip(1)).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("cleverhans-codegen: {message}");
            eprintln!(
                "usage: cleverhans-codegen --schema <registry.json> \
                 [--ts <out.ts>] [--py <out.py>] [--rs <out.rs>] [--check]"
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(ToString::to_string))
    }

    #[test]
    fn requires_schema_flag() {
        assert!(parse(&["--ts", "out.ts"]).is_err());
    }

    #[test]
    fn parses_all_flags() {
        let args =
            parse(&["--schema", "r.json", "--ts", "a.ts", "--py", "b.py"]).expect("valid args");

        assert_eq!(
            (args.schema.as_str(), args.ts.as_deref(), args.py.as_deref()),
            ("r.json", Some("a.ts"), Some("b.py"))
        );
    }

    #[test]
    fn rejects_unknown_and_duplicate_flags() {
        assert!(parse(&["--schema", "r.json", "--wat", "x"]).is_err());
        assert!(parse(&["--schema", "a", "--schema", "b"]).is_err());
    }
}
