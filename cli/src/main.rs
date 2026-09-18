use std::ffi::OsString;

fn main() {
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("madelog: cannot read the current directory: {e}");
            std::process::exit(1);
        }
    };

    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    if let Err(err) = madelog::run_in(&cwd, &args) {
        if let Some(exit) = err.downcast_ref::<madelog::ExitError>() {
            // clap already renders its own help and usage text.
            // Help and version (exit 0) belong on stdout; errors on stderr.
            if exit.code == 0 {
                print!("{}", exit.message);
                std::process::exit(0);
            }
            eprint!(
                "{}{}",
                exit.message,
                if exit.message.ends_with('\n') {
                    ""
                } else {
                    "\n"
                }
            );
            std::process::exit(exit.code);
        }
        eprintln!("madelog: {err:#}");
        std::process::exit(1);
    }
}
