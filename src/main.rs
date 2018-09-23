#[macro_use] extern crate log;
#[macro_use] extern crate serde_derive;

extern crate env_logger;
extern crate failure;
extern crate serde;
extern crate serde_json;

use std::process::{Child, Command, Stdio};

static HLS_ROOT:  &str = "./hls-frags";
static TEST_FILE: &str = "/srv/movies/weeb-test/yzq-01.mkv";

#[derive(Debug, Deserialize)]
struct ProbeResult {
	format: FormatResult,
	streams: Vec<StreamResult>,
}

#[derive(Debug, Deserialize)]
struct FormatResult {
	filename: String,
	nb_streams: usize,
}

#[derive(Debug, Deserialize)]
struct StreamResult {
	index: usize,
	codec_name: String,
	codec_type: String,
}

#[derive(Debug, Deserialize)]
enum CodecType {
	Attachment,
	Audio,
	Video,
	Subtitle,
	Unknown(String),
}

impl<'a> From<&'a str> for CodecType {
	fn from(codec_type: &str) -> Self {
		match codec_type {
			"attachment" => CodecType::Attachment,
			"audio" => CodecType::Audio,
			"video" => CodecType::Video,
			"subtitle" => CodecType::Subtitle,
			_ => CodecType::Unknown(codec_type.to_string()),
		}
	}
}

fn main() -> Result<(), failure::Error> {
	env_logger::init();
	info!("starting HLS stream");

	// theory of operation
	// - sanity check ffmpeg env?
	// - read ffprobe
	// - prompt user to select streams for muxing
	// - start transcoding individual streams
	// - create a master playlist

	let probe = read_streams(TEST_FILE)?;
	info!("got streams:\n {:?}", probe);

	// setup stream tables
	let mut audio_streams  = vec![];
	let mut attach_streams = vec![];
	let mut video_streams  = vec![];
	let mut sub_streams    = vec![];

	for stream in probe.streams {
		let codec_ty = CodecType::from(&stream.codec_type[..]);

		match codec_ty {
			CodecType::Attachment => attach_streams.push(stream),
			CodecType::Audio      => audio_streams.push(stream),
			CodecType::Video      => video_streams.push(stream),
			CodecType::Subtitle   => sub_streams.push(stream),

			unknown => warn!("unknown codec: {:?}", unknown),
		}
	}

	info!("finished reading stream data");
	info!("video\t{}",  video_streams.len());
	info!("audio\t{}",  audio_streams.len());
	info!("subs\t{}",   sub_streams.len());
	info!("attach\t{}", attach_streams.len());

	// TODO return errors
	if video_streams.len() == 0 || audio_streams.len() == 0 {
		warn!("no streams to mux ... exiting");
		return Ok(())
	}


	// TODO select streams to map by UI
	let stream_a = &audio_streams[0];
	let stream_v = &video_streams[0];

	let mut muxer = begin_stream(TEST_FILE, &Profile {
		bitrate_video: String::from("3000k"),
		bitrate_audio: String::from("128k"),
	})?;

	info!("waiting on streams ...");
	muxer.wait();

	info!("all done :-)");
	Ok(())
}

struct Profile {
	bitrate_video: String,
	bitrate_audio: String,
}

fn begin_stream(src_path: &str, prof: &Profile) -> Result<Child, failure::Error> {
	let ffmpeg_result = Command::new("ffmpeg")
		.arg("-y")
		.arg("-re")
		.arg("-i").arg(src_path)
		.arg("-b:v").arg(&prof.bitrate_video)
		.arg("-c:v").arg("libx264")
		.arg("-x264opts").arg("keyint=60:no-scenecut")
		.arg("-profile:v").arg("main")
		.arg("-r").arg("30")
		.arg("-b:a").arg(&prof.bitrate_audio)
		.arg("-c:a").arg("libfdk_aac")
		.arg("-map").arg("0:v")
		.arg("-map").arg("0:a")
		.arg("-hls_list_size").arg("10")
		.arg(&format!("{}/test.m3u8", HLS_ROOT))
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()?;

	Ok(ffmpeg_result)
}

fn read_streams(input_path: &str) -> Result<ProbeResult, failure::Error> {
	let ffprobe_result = Command::new("ffprobe")
		.arg("-of").arg("json")
		.arg("-show_format").arg("-show_streams")
		.arg(input_path)
		.output()?;

	let ffprobe_buf  = String::from_utf8(ffprobe_result.stdout)?;
	let ffprobe_json: ProbeResult = serde_json::from_str(&ffprobe_buf)?;

	Ok(ffprobe_json)
}
