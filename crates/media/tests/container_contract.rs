use std::fs;
use std::path::Path;

#[test]
fn runner_image_owns_the_yt_dlp_executable() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dockerfile = fs::read_to_string(manifest_dir.join("../../Dockerfile"))
        .expect("workspace Dockerfile must be readable");
    let service = dockerfile
        .split("FROM runtime-common AS service")
        .nth(1)
        .and_then(|tail| tail.split("FROM runtime-base AS runner-packages").next())
        .expect("service stage must exist");
    let runner = dockerfile
        .split("FROM runner-packages AS runner")
        .nth(1)
        .expect("runner stage must exist");
    let copy = "COPY --from=yt-dlp --chown=65532:65532 /usr/local/bin/yt-dlp /usr/local/bin/yt-dlp";

    assert!(!service.contains(copy), "service must not contain yt-dlp");
    assert!(runner.contains(copy), "runner must contain yt-dlp");
}

#[test]
fn runner_image_owns_chrome_headless_shell() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dockerfile = fs::read_to_string(manifest_dir.join("../../Dockerfile"))
        .expect("workspace Dockerfile must be readable");
    let service = dockerfile
        .split("FROM runtime-common AS service")
        .nth(1)
        .and_then(|tail| tail.split("FROM runtime-base AS runner-packages").next())
        .expect("service stage must exist");
    let runner = dockerfile
        .split("FROM runner-packages AS runner")
        .nth(1)
        .expect("runner stage must exist");
    let copy = "COPY --from=chrome-headless-shell --chown=65532:65532 /usr/local/lib/chrome-headless-shell /usr/local/lib/chrome-headless-shell";

    assert!(
        !service.contains("chrome-headless-shell"),
        "service must not contain chrome-headless-shell"
    );
    assert!(
        !service.contains("chromium"),
        "service must not mention chromium"
    );
    assert!(
        runner.contains(copy),
        "runner must contain chrome-headless-shell"
    );
    assert!(
        runner.contains("USER 65532:65532"),
        "runner must keep the non-root media user after copying chrome-headless-shell"
    );
    assert!(
        dockerfile.contains("version=152.0.7977.54"),
        "amd64 chrome-headless-shell must stay on Chrome for Testing Stable 152"
    );
    assert!(
        !dockerfile.contains("153.0.8010.5"),
        "runner must not pin an arm64 Beta chrome-headless-shell"
    );
}
