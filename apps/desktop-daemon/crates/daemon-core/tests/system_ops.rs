use daemon_core::system_ops::{
    fetch_network, kill_process, list_processes, watch_filesystem, FsWatchRequest,
    NetworkFetchRequest, ProcessKillRequest, ProcessListRequest,
};
use std::path::PathBuf;

#[tokio::test]
async fn process_listing_honours_a_small_limit() {
    let result = list_processes(ProcessListRequest {
        query: None,
        limit: Some(1),
    })
    .await
    .expect("the host process listing should be available");
    assert!(result.processes.len() <= 1);
}

#[tokio::test]
async fn process_kill_requires_exactly_one_selector() {
    let error = kill_process(ProcessKillRequest {
        pid: None,
        name: None,
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("exactly one"));

    let error = kill_process(ProcessKillRequest {
        pid: Some(1),
        name: Some("init".into()),
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("exactly one"));
}

#[tokio::test]
async fn network_fetch_rejects_non_http_schemes() {
    let error = fetch_network(NetworkFetchRequest {
        url: "file:///etc/passwd".into(),
        method: "GET".into(),
        headers: Default::default(),
        body: None,
        timeout_ms: Some(100),
        max_bytes: Some(100),
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("http and https"));
}

#[tokio::test]
async fn filesystem_watch_reports_invalid_paths_without_hanging() {
    let error = watch_filesystem(FsWatchRequest {
        path: PathBuf::from("this-path-does-not-exist-synthhires"),
        duration_ms: Some(100),
        recursive: false,
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("watch path") || error.to_string().contains("watcher"));
}
