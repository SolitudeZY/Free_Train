use std::{env, process::ExitCode};

fn main() -> ExitCode {
    let Some(source) = env::args().nth(1) else {
        eprintln!("usage: cargo run -p media --example probe -- <media-path>");
        return ExitCode::FAILURE;
    };

    match media::probe_media("ffprobe", &source) {
        Ok(document) => {
            let video = document
                .streams
                .iter()
                .find(|stream| stream.codec_type.as_deref() == Some("video"));
            println!("source={source}");
            println!("streams={}", document.streams.len());
            if let Some(video) = video {
                println!(
                    "video={}x{} codec={} frame_rate={}",
                    video.width.unwrap_or_default(),
                    video.height.unwrap_or_default(),
                    video.codec_name.as_deref().unwrap_or("unknown"),
                    video.avg_frame_rate.as_deref().unwrap_or("unknown")
                );
            }
            if let Some(format) = document.format {
                println!(
                    "duration={}",
                    format.duration.as_deref().unwrap_or("unknown")
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}
