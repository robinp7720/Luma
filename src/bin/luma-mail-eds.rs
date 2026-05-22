use anyhow::Result;
use clap::{Parser, Subcommand};

#[path = "../mail_eds_protocol.rs"]
mod mail_eds_protocol;

#[path = "../mail_eds_ffi.rs"]
mod mail_eds_ffi;

#[path = "../mail_eds.rs"]
mod mail_eds;

use mail_eds_protocol::{
    MailEdsActionRequest, MailEdsSearchRequest, MailEdsSearchResponse, MailEdsStatus,
};

#[derive(Parser, Debug)]
#[command(name = "luma-mail-eds")]
#[command(about = "EDS-backed Evolution mail helper for Luma")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Search {
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    Open {
        #[arg(long)]
        message_id: String,
    },
    Reply {
        #[arg(long)]
        message_id: String,
    },
    Compose {
        #[arg(long)]
        message_id: String,
    },
    CopySender {
        #[arg(long)]
        message_id: String,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Search { query, limit } => {
            let request = MailEdsSearchRequest { query, limit };
            let results = mail_eds::search_mail(&request.query, request.limit)?;
            let response = MailEdsSearchResponse {
                ok: true,
                message: String::new(),
                results,
            };
            println!("{}", serde_json::to_string(&response)?);
        }
        Command::Open { message_id } => {
            let request = MailEdsActionRequest { message_id };
            let status = mail_eds::open_message(&request.message_id)?;
            emit_status(status)?;
        }
        Command::Reply { message_id } => {
            let request = MailEdsActionRequest { message_id };
            let status = mail_eds::reply_to_message(&request.message_id)?;
            emit_status(status)?;
        }
        Command::Compose { message_id } => {
            let request = MailEdsActionRequest { message_id };
            let status = mail_eds::compose_to_message(&request.message_id)?;
            emit_status(status)?;
        }
        Command::CopySender { message_id } => {
            let request = MailEdsActionRequest { message_id };
            let status = mail_eds::copy_sender(&request.message_id)?;
            emit_status(status)?;
        }
    }
    Ok(())
}

fn emit_status(status: MailEdsStatus) -> Result<()> {
    println!("{}", serde_json::to_string(&status)?);
    Ok(())
}
