use rustls::{Certificate, PrivateKey, ServerConfig};
use std::{fs, io, net::SocketAddr, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsAcceptor;

struct Config {
	cert_and_key: Option<(Vec<u8>, Vec<u8>)>,
	port: u16,
	host: String,
	base_url: String,
}

fn cli_args() -> Result<Config, ees::Error> {
	let mut config = Config {
		cert_and_key: None,
		port: 1965,
		host: "127.0.0.1".into(),
		base_url: "gemini://localhost/".into(),
	};
	let mut args = std::env::args_os().skip(1);
	let mut cert = None;
	let mut key = None;
	while let Some(arg) = args.next() {
		match arg.to_str() {
			Some("--cert") => {
				let path = args
					.next()
					.ok_or(ees::err!("Please provide a path to a certificate file"))?;
				let mut file = fs::File::open(path)?;
				let mut data = vec![];
				io::Read::read_to_end(&mut file, &mut data)?;
				cert = Some(data);
			}
			Some("--key") => {
				let path = args
					.next()
					.ok_or(ees::err!("Please provide a path to a key file"))?;
				let mut file = fs::File::open(path)?;
				let mut data = vec![];
				io::Read::read_to_end(&mut file, &mut data)?;
				key = Some(data);
			}
			Some("--port") => {
				config.port = args
					.next()
					.ok_or(ees::err!("Please provide a port number"))?
					.to_str()
					.ok_or(ees::err!("Please provide a valid port number"))?
					.parse()
					.map_err(|_| ees::err!("Please provide a valid port number"))?;
			}
			Some("--host") => {
				config.host = args
					.next()
					.ok_or(ees::err!("Please provide a hostname"))?
					.to_str()
					.ok_or(ees::err!("Please provide a valid hostname"))?
					.to_string();
			}
			Some("--base-url") => {
				config.base_url = args
					.next()
					.ok_or(ees::err!("Please provide a hostname"))?
					.to_str()
					.ok_or(ees::err!("Please provide a valid hostname"))?
					.to_string();
			}
			Some("-h" | "--help") => {
				ees::bail!(
					"Usage:
fend-gemini [options]

Options:
    --cert <path>     Path to a certificate file (default: disable TLS)
    --key <path>      Path to a key file (default: disable TLS)
    --port <port>     Port to listen on (default: 1965)
    --host <host>     Hostname to listen on (default: 127.0.0.1)
    --base-url <url>  Base URL for which to respond"
				);
			}
			_ => {
				ees::bail!("Unknown argument: {}", arg.to_string_lossy());
			}
		}
	}
	match (cert, key) {
		(Some(cert), Some(key)) => {
			config.cert_and_key = Some((cert, key));
		}
		(None, None) => (),
		_ => {
			ees::bail!("Please provide both a certificate and a key");
		}
	}
	Ok(config)
}

#[tokio::main]
async fn main() -> ees::MainResult {
	let config = cli_args()?;
	let listener = tokio::net::TcpListener::bind((config.host, config.port)).await?;
	let tls_acceptor = if let Some((cert, key)) = config.cert_and_key {
		let chain = rustls_pemfile::certs(&mut io::Cursor::new(cert))?
			.into_iter()
			.map(Certificate)
			.collect();
		let mut key = rustls_pemfile::pkcs8_private_keys(&mut io::Cursor::new(key))?;
		if key.len() != 1 {
			ees::bail!(
				"Specified key file contains {} keys (1 key required)",
				key.len()
			);
		}
		let key = PrivateKey(key.remove(0));
		let config = Arc::new(
			ServerConfig::builder()
				.with_safe_defaults()
				.with_no_client_auth()
				.with_single_cert(chain, key)?,
		);
		Some(TlsAcceptor::from(config))
	} else {
		None
	};
	loop {
		let (stream, client_ip) = listener.accept().await?;
		eprintln!("accepted connection from {}", client_ip);
		let tls_acceptor = tls_acceptor.clone();
		let base_url = config.base_url.clone();
		tokio::spawn(async move {
			if let Some(tls_acceptor) = tls_acceptor {
				match tls_acceptor.accept(stream).await {
					Err(e) => eprintln!("TLS handshake failed: {}", e),
					Ok(tls_stream) => {
						if let Err(e) = accept(client_ip, tls_stream, &base_url).await {
							eprintln!("Error: {}", e);
						}
					}
				}
			} else {
				if let Err(e) = accept(client_ip, stream, &base_url).await {
					eprintln!("Error: {}", e);
				}
			}
		})
		.await?;
	}
}

async fn accept(
	client_ip: SocketAddr,
	stream: impl tokio::io::AsyncRead + tokio::io::AsyncWrite,
	base_url: &str,
) -> Result<(), ees::Error> {
	let (mut reader, mut writer) = tokio::io::split(stream);
	eprintln!("entering accept()");
	let mut buffer = Vec::with_capacity(1024);
	reader.read_buf(&mut buffer).await?;
	let input_string = String::from_utf8(buffer)?;
	println!("received request from {client_ip}: {input_string}");
	let url = url::Url::parse(&input_string)?;
	if url.scheme() != "gemini" {
		writer.write_all(b"50 Invalid URL scheme").await?;
		return Ok(());
	}
	eprintln!("correct url scheme");
	match url.query() {
		Some(query) => {
			eprintln!("query");
			let decoded = percent_encoding::percent_decode_str(query).decode_utf8()?;
			let response = fend_core::evaluate(&decoded, &mut fend_core::Context::new())?;
			let response = format!(
				"20 text/gemini; charset=utf-8; lang=en\r\n# fend-gemini\nInput: {}\nResult: {}\n=> {} Make another calculation",
                decoded,
				response.get_main_result(),
                base_url,
			);
			writer.write_all(response.as_bytes()).await?;
		}
		None => {
			eprintln!("no query: returning prompt");
			writer.write_all(b"10 Enter a calculation\r\n").await?;
		}
	}
	Ok(())
}
