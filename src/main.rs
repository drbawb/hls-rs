#[macro_use] extern crate failure;
#[macro_use] extern crate log;
#[macro_use] extern crate serde_derive;

extern crate clap;
extern crate env_logger;
extern crate serde;
extern crate serde_json;

use clap::{Arg, App, SubCommand};
use std::io;
use std::process::{Child, Command, Stdio};

static HLS_ROOT:  &str = "/srv/hls";

/// Represents a user's selection of streams
#[derive(Debug)]
struct MuxSettings {
	av_path: String,
	st_path: Option<String>,

	idx_a:  usize,
	idx_v:  usize,
	ids_st: Option<usize>,
}

struct Profile {
	level_name: &'static str,
	bitrate_video: String,
	bitrate_audio: String,
}

/// Table of individual sub-streams for a given set of inputs
#[derive(Debug, Deserialize)]
struct StreamTable {
	video:  Vec<StreamResult>,
	audio:  Vec<StreamResult>,
	attach: Vec<StreamResult>,
	subs:   Vec<StreamResult>,
}

impl StreamTable {
	pub fn new() -> Self {
		StreamTable {
			video:  vec![],
			audio:  vec![],
			attach: vec![],
			subs:   vec![],
		}
	}
}


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

#[derive(Debug, Fail)]
enum CliError {
	#[fail(display = "missing input file")]	
	MissingInput,
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

	// read command line input
	let matches = App::new("hls-rs")
		.version("0.0")
		.author("hime@localhost")
		.about("Opens an AV stream and creates a series of realtime HLS playlists.")
		.arg(Arg::with_name("INPUT")
			 .help("The main input file, containing at least one video & audio track.")
			 .required(true)
			 .index(1))
		.arg(Arg::with_name("SUBTITLE")
			 .help("A secondary input file containing at least one subtitle track, and any number of attachments.")
			 .required(false)
			 .index(2))
		.get_matches();

	let input_av = matches.value_of("INPUT")
		.ok_or(CliError::MissingInput)?;


	let input_subs = matches.value_of("SUBTITLE");

	info!("probing input file: {}", input_av);

	let probe_av = read_streams(input_av)?;
	info!("got streams:\n {:?}", probe_av);

	// setup stream tables
	let mut streams = StreamTable::new();

	// read the av file
	for stream in probe_av.streams {
		let codec_ty = CodecType::from(&stream.codec_type[..]);

		match codec_ty {
			CodecType::Audio      => streams.audio.push(stream),
			CodecType::Video      => streams.video.push(stream),

			// read these later
			CodecType::Attachment => debug!("av: ignoring attachment"),
			CodecType::Subtitle   => debug!("av: subtitle track"),

			unknown => warn!("unknown codec: {:?}", unknown),
		}
	}

	// read the subs file
	info!("probing subs: {:?}", input_subs);
	if let Some(input_subs) = input_subs {
		let probe_av = read_streams(input_subs)?;
		info!("got st streams:\n {:?}", probe_av);

		for stream in probe_av.streams {
			let codec_ty = CodecType::from(&stream.codec_type[..]);

			match codec_ty {
				CodecType::Audio      => debug!("sub: ignoring audio"),
				CodecType::Video      => debug!("sub: ignoring video"),

				// read these later
				CodecType::Attachment => streams.attach.push(stream),
				CodecType::Subtitle   => streams.subs.push(stream),

				unknown => warn!("unknown codec: {:?}", unknown),
			}
		}	
	}

	info!("finished reading stream data");
	info!("video\t{}", streams.video.len());
	info!("audio\t{}", streams.audio.len()); 
	info!("subs\t{}",  streams.subs.len()); 
	info!("attach\t{}",streams.attach.len()); 

	fn select_stream_idx(name: &str, streams: &Vec<StreamResult>) -> Result<usize, io::Error> {
	
		println!("select {} track:", name);	
		for (idx, stream) in streams.iter().enumerate() {
			println!("{}: {}", idx, stream.codec_name);
		}

		let mut buf = String::new();
		loop {
			buf.truncate(0);
			io::stdin().read_line(&mut buf)?;
			match buf.trim().parse() {
				Ok(num) => return Ok(num),
				Err(msg) => warn!("err: {}", msg),
			}
		}
	}

	let mut mux_settings = MuxSettings {
		av_path: input_av.to_string(),
		st_path: input_subs.map(|x| x.to_string()),

		idx_a:  0,
		idx_v:  0,
		ids_st: None,
	};

	mux_settings.idx_v = select_stream_idx("video", &streams.video)?;
	mux_settings.idx_a = select_stream_idx("audio", &streams.audio)?;

	// prompt user to select subtitle stream if its loaded
	if mux_settings.st_path.is_some() {
		mux_settings.ids_st = Some(select_stream_idx("subs", &streams.subs)?);	
	}

	let mut muxer_src = begin_stream(&mux_settings, &Profile {
		level_name: "cdn00_src",
		bitrate_video: String::from("3000k"),
		bitrate_audio: String::from("192k"),
	})?;

	let mut muxer_mid = begin_stream(&mux_settings, &Profile {
		level_name: "cdn00_mid",
		bitrate_video: String::from("2250k"),
		bitrate_audio: String::from("128k"),
	})?;

	let mut muxer_low = begin_stream(&mux_settings, &Profile {
		level_name: "cdn00_low",
		bitrate_video: String::from("960k"),
		bitrate_audio: String::from("96k"),
	})?;

	info!("waiting on streams ...");
	write_master_playlist()?;
	
	muxer_src.wait()?;
	muxer_mid.wait()?;
	muxer_low.wait()?;

	info!("all done :-)");
	Ok(())
}

fn write_master_playlist() -> Result<(), failure::Error> {
	use std::fs::File;
	use std::io::Write;

	let mut pl = File::create(format!("{}/cdn00.m3u8", HLS_ROOT))?;

	// write the HLS header
	writeln!(pl, "#EXTM3U")?;
	writeln!(pl, "#EXT-X-VERSION:3")?;

	// write renditions 
	writeln!(pl, "#EXT-X-STREAM-INF:BANDWIDTH=4000000,RESOLUTION=1920x1080")?;
	writeln!(pl, "cdn00_src/index.m3u8")?;

	writeln!(pl, "#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1920x1080")?;
	writeln!(pl, "cdn00_mid/index.m3u8")?;

	// write source rendition
	writeln!(pl, "#EXT-X-STREAM-INF:BANDWIDTH=960000,RESOLUTION=1920x1080")?;
	writeln!(pl, "cdn00_low/index.m3u8")?;

	Ok(())
}

fn begin_stream(src: &MuxSettings, prof: &Profile) -> Result<Child, failure::Error> {
	let seg_path = format!("{}/{}/index.m3u8", HLS_ROOT, prof.level_name);
	let seg_name = format!("{}/{}/%03d.ts", HLS_ROOT, prof.level_name);

	let mut ffmpeg_cmd = Command::new("ffmpeg");

	// setup basic video streaming properties
	ffmpeg_cmd
		.arg("-y")
		.arg("-re")
		.arg("-i").arg(&src.av_path)
		.arg("-b:v").arg(&prof.bitrate_video)
		.arg("-c:v").arg("libx264")
		.arg("-x264opts").arg("keyint=300:no-scenecut")
		.arg("-pix_fmt").arg("yuv420p")
		.arg("-profile:v").arg("main")
		.arg("-r").arg("30")
		.arg("-b:a").arg(&prof.bitrate_audio)
		.arg("-c:a").arg("libfdk_aac")
		.arg("-preset").arg("veryfast")
		.arg("-map").arg(&format!("v:{}", src.idx_v))
		.arg("-map").arg(&format!("a:{}", src.idx_a));
		

	// TODO: PGM subtitles?
	// add in subtitles
	if let (&Some(ref st_path), &Some(ref st_idx)) = (&src.st_path, &src.ids_st) {
		ffmpeg_cmd
			.arg("-vf").arg(&format!("subtitles={}:si={}", st_path, st_idx));
	}


	// set up HLS output options
	ffmpeg_cmd
		.arg("-hls_list_size").arg("10")
		.arg("-hls_time").arg("10")
		.arg("-hls_flags").arg("delete_segments")
		.arg("-hls_segment_filename").arg(seg_name)
		.arg(&seg_path);

	info!("about to run: {:?}", ffmpeg_cmd);

	let ffmpeg_result = ffmpeg_cmd
		.stdout(Stdio::null())
		.stderr(Stdio::null())
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
