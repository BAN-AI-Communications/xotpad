use clap::{Arg, Command as ClapCommand};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use pty_process::Command as _;
use regex::Regex;
use std::env;
use std::str::FromStr;
use tokio::io::{split, stdin, stdout};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio_util::codec::Framed;

use xotpad::pad::{HostPad, UserPad};
use xotpad::x121::X121Address;
use xotpad::x25::{X25CallRequest, X25Modulo, X25Parameters, X25VirtualCircuit};
use xotpad::xot;
use xotpad::xot::{XotCodec, XotResolver};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = ClapCommand::new("xotpad")
        .arg(
            Arg::new("address")
                .short('a')
                .takes_value(true)
                .value_name("address")
                .help("Local X.121 address"),
        )
        .arg(
            Arg::new("xot_gateway")
                .short('g')
                .takes_value(true)
                .value_name("host")
                .help("XOT gateway"),
        )
        .arg(
            Arg::new("x25_profile")
                .short('P')
                .takes_value(true)
                .value_name("profile")
                .help("X.25 profile"),
        )
        .arg(
            Arg::new("listen")
                .short('l')
                .help("Listen for incoming calls"),
        )
        .arg(
            Arg::new("accept")
                .short('L')
                .conflicts_with("listen")
                .help("Accept incoming calls"),
        )
        .arg(
            Arg::new("call_address")
                .required(false)
                .index(1)
                .value_name("address")
                .conflicts_with("listen")
                .conflicts_with("accept")
                .help("X.121 address to call"),
        )
        .get_matches();

    let x25_parameters = if matches.is_present("x25_profile") {
        match get_x25_profile(matches.value_of("x25_profile").unwrap()) {
            Some(parameters) => parameters,
            None => {
                return Err("TODO - profile not found...".into());
            }
        }
    } else {
        X25Parameters::default()
    };

    let should_listen = matches.is_present("listen");
    let should_accept = matches.is_present("accept");

    if should_listen {
        let mut listen_table = ListenTable::new();

        listen_table.register("^737202..$", "/home/andrew/tmp/inf0.py".into())?;

        let listener = TcpListener::bind(("0.0.0.0", xot::TCP_PORT)).await?;

        loop {
            let (tcp_stream, tcp_address) = listener.accept().await?;

            println!("got a connection from {}", tcp_address);

            let link = Framed::new(tcp_stream, XotCodec::new());

            let (mut circuit, call_request) =
                X25VirtualCircuit::wait_for_call(link, &x25_parameters).await?;

            let command = listen_table.lookup(&call_request);

            if command.is_none() {
                circuit.clear_call(0, Some(0)).await?;
                continue;
            }

            let command = command.unwrap();

            let command: Vec<&str> = command.split_whitespace().collect();

            let mut child = Command::new(command[0])
                .args(&command[1..])
                .spawn_pty(None)?;

            let (a, b) = split(child.pty_mut());

            circuit.accept_call().await?;

            let pad = HostPad::new(a, b, circuit);

            pad.run().await?;

            // If we get here and the child process is still running... kill it!
            if let Err(e) = child.kill().await {
                println!("I tried killing, but got {:?}", e);
            }

            let exit_status = child.wait().await?;

            println!("think we are done, exited with {:?}", exit_status);
        }
    } else {
        let address = matches
            .value_of("address")
            .map(|address| address.to_string())
            .or_else(|| env::var("X121_ADDRESS").ok())
            .unwrap_or_else(|| "".to_string());

        let address = X121Address::from_str(&address)?;

        let xot_gateway = matches
            .value_of("xot_gateway")
            .map(|gateway| gateway.to_string())
            .or_else(|| env::var("XOT_GATEWAY").ok());

        let mut xot_resolver = XotResolver::new();

        if let Some(xot_gateway) = xot_gateway {
            xot_resolver.add("", xot_gateway);
        } else {
            // TODO...
            xot_resolver.add("^(...)(...)..$", r"\2.\1.x25.org".into());
        }

        let call_address = matches
            .value_of("call_address")
            .map(X121Address::from_str)
            .transpose()?;

        let listener = if call_address.is_none() && should_accept {
            match TcpListener::bind(("0.0.0.0", xot::TCP_PORT)).await {
                Ok(l) => Some(l),
                Err(e) => {
                    eprintln!("xotpad: unable to listen on {}: {}", xot::TCP_PORT, e);
                    None
                }
            }
        } else {
            None
        };

        let mut pad = UserPad::new(
            stdin(),
            stdout(),
            &x25_parameters,
            &address,
            &xot_resolver,
            listener,
            call_address.is_some(),
        );

        if let Some(call_address) = call_address {
            let call_data = "".as_bytes();

            pad.call(&call_address, call_data).await?;

            // TODO: we need to check if this was successful, if not exit!
        }

        enable_raw_mode()?;

        pad.run().await?;

        disable_raw_mode()?;
    }

    Ok(())
}

fn get_x25_profile(name: &str) -> Option<X25Parameters> {
    match name {
        "default8" => Some(X25Parameters::default_with_modulo(X25Modulo::Normal)),
        "default128" => Some(X25Parameters::default_with_modulo(X25Modulo::Extended)),
        _ => None,
    }
}

struct ListenTable {
    targets: Vec<(Regex, String)>,
}

impl ListenTable {
    fn new() -> Self {
        Self {
            targets: Vec::new(),
        }
    }

    fn register(&mut self, called_address: &str, command: String) -> Result<(), String> {
        let called_address = Regex::new(called_address).unwrap();

        self.targets.push((called_address, command));

        Ok(())
    }

    fn lookup(&self, call_request: &X25CallRequest) -> Option<String> {
        let called_address = call_request.called_address.to_string();

        for (called_address_expression, command) in self.targets.iter() {
            if !called_address_expression.is_match(&called_address) {
                continue;
            }

            // TODO: match on CUD...

            return Some(command.clone());
        }

        None
    }
}

impl Default for ListenTable {
    fn default() -> Self {
        ListenTable::new()
    }
}
