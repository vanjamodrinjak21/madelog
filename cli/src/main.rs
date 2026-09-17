use std::ffi::OsString;

fn main() {
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("pop: cannot read the current directory: {e}");
            std::process::exit(1);
        }
    };

    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    if let Err(err) = pop::run_in(&cwd, &args) {
        if let Some(exit) = err.downcast_ref::<pop::ExitError>() {
            // clap already renders its own help and usage text.
            eprint!(
                "{}{}",
                exit.message,
                if exit.message.ends_with('\n') { "" } else { "\n" }
            );
            std::process::exit(exit.code);
        }
        eprintln!("pop: {err:#}");
        std::process::exit(1);
    }
}
