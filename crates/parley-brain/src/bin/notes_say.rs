//! Read a transcript file and print the notes a chosen adapter writes for it.

use clap::{Parser, ValueEnum};
use parley_brain::{Line, Transcript};
use parley_llm::{LlmProvider, anthropic::Anthropic, openai::OpenAiCompatible};
use std::path::PathBuf;

#[derive(Clone, ValueEnum)]
enum Adapter {
    Openai,
    Anthropic,
}

#[derive(Parser)]
#[command(
    about = "Print summary and action items for a transcript file (config from env, see README)"
)]
struct Args {
    #[arg(long, value_enum)]
    adapter: Adapter,
    /// One line per utterance: `Speaker: text`.
    transcript: PathBuf,
}

fn parse(src: &str) -> Result<Transcript, String> {
    let mut t = Transcript::new();
    for (i, raw) in src
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let (speaker, text) = raw
            .split_once(':')
            .ok_or_else(|| format!("line {}: expected `Speaker: text`", i + 1))?;
        // The file has no timings: line number stands in for the start time.
        t.push(Line {
            speaker: speaker.trim().into(),
            start_ms: i as u64 * 1000,
            text: text.trim().into(),
        });
    }
    Ok(t)
}

async fn run<P: LlmProvider>(p: P, t: &Transcript) -> Result<(), Box<dyn std::error::Error>> {
    let n = t.notes(&p).await?;
    println!("Summary:\n{}\n\nAction items:", n.summary);
    if n.action_items.is_empty() {
        println!("(none)");
    }
    for a in n.action_items {
        match a.owner {
            Some(o) => println!("- {} ({o})", a.text),
            None => println!("- {}", a.text),
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let t = parse(&std::fs::read_to_string(&args.transcript)?)?;
    match args.adapter {
        Adapter::Openai => run(OpenAiCompatible::from_env()?, &t).await,
        Adapter::Anthropic => run(Anthropic::from_env()?, &t).await,
    }
}
