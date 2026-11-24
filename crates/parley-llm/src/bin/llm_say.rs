//! Send one prompt through a chosen adapter and print the stream.

use clap::{Parser, ValueEnum};
use futures_util::StreamExt;
use parley_llm::{
    ChatRequest, Delta, LlmProvider, Message, anthropic::Anthropic, openai::OpenAiCompatible,
};
use std::io::Write;

#[derive(Clone, ValueEnum)]
enum Adapter {
    Openai,
    Anthropic,
}

#[derive(Parser)]
#[command(
    about = "Send one prompt through an LLM adapter and print the stream (config from env, see README)"
)]
struct Args {
    #[arg(long, value_enum)]
    adapter: Adapter,
    /// Optional system prompt.
    #[arg(long)]
    system: Option<String>,
    prompt: String,
}

async fn run<P: LlmProvider>(p: P, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let req = ChatRequest {
        system: args.system.clone(),
        messages: vec![Message::user(&args.prompt)],
        ..Default::default()
    };
    let mut s = p.stream(req).await?;
    while let Some(d) = s.next().await {
        match d? {
            Delta::Text(t) => {
                print!("{t}");
                std::io::stdout().flush()?;
            }
            Delta::ToolCall {
                id,
                name,
                arguments,
            } => println!("\n[tool call {id}] {name} {arguments}"),
            Delta::End(r) => println!("\n[end: {r:?}]"),
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    match args.adapter {
        Adapter::Openai => run(OpenAiCompatible::from_env()?, &args).await,
        Adapter::Anthropic => run(Anthropic::from_env()?, &args).await,
    }
}
