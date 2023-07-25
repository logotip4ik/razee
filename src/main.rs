use async_recursion::async_recursion;
use flate2::read::GzDecoder;
use futures::future::join_all;
use node_semver::{Range, Version};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fs,
    io::{BufReader, Cursor},
    path::Path,
    sync::Arc,
};
use tar::Archive;
use tokio::sync::Mutex;
use walkdir::WalkDir;

use http_client::HttpClient;

mod http_client;
mod logger;

type DependenciesMap = HashMap<String, String>;
type ProcessedDeps = Arc<Mutex<HashMap<String, Dep>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryPackage {
    name: String,
    time: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Package {
    name: String,
    dependencies: Option<DependenciesMap>,
    #[serde(rename = "devDependencies")]
    dev_dependencies: Option<DependenciesMap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DependencyDist {
    integrity: String,
    tarball: String,
    #[serde(rename = "fileCount")]
    file_count: Option<i16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Dependency {
    name: String,
    version: String,
    dependencies: Option<DependenciesMap>,
    #[serde(rename = "devDependencies")]
    dev_dependencies: Option<DependenciesMap>,
    dist: DependencyDist,
}

#[derive(Debug, Clone)]
struct Dep {
    name: String,
    version: String,
}

const REGISTRY_URL: &str = "https://registry.npmjs.org";
const NODE_MODULES: &str = "node_modules";

fn parse_root_package() -> Package {
    let mut package_path = env::current_dir().expect("cannot get current dir");

    package_path.push("package.json");

    if !package_path.exists() {
        panic!("no package json exists")
    }

    let package_json = fs::File::open(package_path).expect("cannot open package.json");
    let reader = BufReader::new(package_json);

    let package = serde_json::from_reader(reader).expect("cannot parse package.json");

    return package;
}

fn resolve_version(package: &RegistryPackage, requested_version: &Range) -> Version {
    let dep_versions = package
        .time
        .keys()
        .filter(|version| version.contains("."))
        .map(|v| Version::parse(v).unwrap());

    let satisfied_version = dep_versions
        .clone()
        .find(|version| requested_version.satisfies(version));

    if let Some(version) = satisfied_version {
        return version;
    } else {
        let versions: Vec<Version> = dep_versions.collect();

        if versions.len() == 1 {
            return versions
                .get(0)
                .expect("there is no versions available")
                .clone();
        } else {
            return versions
                .iter()
                .max()
                .unwrap_or({
                    let msg = format!("no versions {:?}\n{:?}", package, versions);

                    versions.get(0).expect(msg.as_str())
                })
                .clone();
        }
    }
}

async fn fetch_dep(dep: &Dep, client: Arc<HttpClient>) -> Dependency {
    let package = client.fetch_package(&dep).await;

    let normalized_version;

    // TODO: export this into function
    if dep.version.starts_with("npm") {
        let package_or_version = dep.version.strip_prefix("npm:").unwrap();

        if package_or_version.contains("@") {
            normalized_version = package_or_version.split("@").last().unwrap();
        } else {
            normalized_version = package_or_version;
        }
    } else {
        normalized_version = dep.version.as_str();
    }

    let requested_version = Range::parse(normalized_version).expect(
        format!(
            "cannot parse requested version: {}:{}",
            package.name, dep.version
        )
        .as_str(),
    );

    let resolved_version = resolve_version(&package, &requested_version);

    let url: String = format!("{REGISTRY_URL}/{}/{resolved_version}", dep.name);
    let dependency = client.fetch_dependency(&url).await;

    return dependency.to_owned();
}

async fn download_tarball(
    dep_name: &String,
    dep_dist: &DependencyDist,
    client: Arc<HttpClient>,
) {
    let dep_dir = format!("{NODE_MODULES}/{dep_name}");

    if Path::new(&dep_dir).exists() {
        if let Some(file_count) = dep_dist.file_count {
            let mut file_counter = 0;

            for entry in WalkDir::new(&dep_dir) {
                let entry = entry.unwrap();

                if entry.file_type().is_file() {
                    file_counter += 1;
                }
            }

            if file_counter == file_count {
                return;
            }
        }
    }

    let tarball_bytes = client.fetch_tarball(dep_dist).await;

    let tarball_cursor = Cursor::new(tarball_bytes);
    let tarball = GzDecoder::new(tarball_cursor);

    let mut archive = Archive::new(tarball);

    for entry in archive.entries().unwrap() {
        if let Ok(mut entry) = entry {
            let mut path = entry
                .path()
                .unwrap()
                .to_str()
                .unwrap()
                .replace("package", &dep_dir)
                .to_owned();

            // Transforms @types/estree   estree/readme
            //              dep_name        entry ? why not package ? idk
            if !path.starts_with(&NODE_MODULES) {
                path = format!("{dep_dir}/{path}");

                let mut path_parts = path.split("/");

                let mut new_path = vec![];

                new_path.push(path_parts.next().unwrap().to_string());

                for part in path_parts {
                    let prev_part = new_path.last().unwrap();

                    if !part.eq(prev_part) {
                        new_path.push(part.to_string());
                    }
                }

                path = new_path.join("/");
            }

            let mut folders: Vec<&str> = path.split("/").collect();
            folders.pop();

            if folders.len() > 1 {
                fs::create_dir_all(folders.join("/")).unwrap();
            }

            if !Path::new(&path).exists() {
                entry.unpack(&path).unwrap();
            }
        }
    }
}

#[async_recursion(?Send)]
async fn process_dep(dep: &Dep, processed_deps: ProcessedDeps, client: Arc<HttpClient>) {
    let package = fetch_dep(&dep, client.clone()).await;
    let tarball_promise = download_tarball(&package.name, &package.dist, client.clone());

    {
        let mut processed = processed_deps.lock().await;

        logger::log_processed(&dep.name);

        processed.insert(dep.name.clone(), dep.clone());
    }

    let mut needs_processing = vec![];

    if let Some(deps) = package.dependencies {
        let processed = processed_deps.lock().await;

        for (k, v) in deps.iter() {
            if !processed.contains_key(k) {
                needs_processing.push(Dep {
                    name: k.to_owned(),
                    version: v.to_owned(),
                });
            }
        }
    }

    tarball_promise.await;

    join_all(
        needs_processing
            .iter()
            .map(|dep| process_dep(dep, processed_deps.clone(), client.clone()))
            .collect::<Vec<_>>(),
    )
    .await;
}

#[tokio::main]
async fn main() {
    let package = parse_root_package();

    let mut needs_processing = vec![];
    let processed_deps: ProcessedDeps = Arc::new(Mutex::new(HashMap::new()));

    if let Some(normal_deps) = package.dependencies {
        normal_deps.into_iter().for_each(|(name, version)| {
            let dep = Dep { name, version };

            needs_processing.push(dep);
        });
    }

    if let Some(dev_deps) = package.dev_dependencies {
        dev_deps.into_iter().for_each(|(name, version)| {
            let dep = Dep { name, version };

            needs_processing.push(dep);
        });
    }

    println!();

    // let http_client = Arc::new(Mutex::new(HttpClient::new()));
    let mut http_client = Arc::new(HttpClient::new());

    join_all(
        needs_processing
            .iter()
            .map(|dep| process_dep(dep, processed_deps.clone(), http_client.clone()))
            .collect::<Vec<_>>(),
    )
    .await;

    let processed = processed_deps.lock().await;

    println!("Fetched {} packages", processed.len());
    // println!("{:?}", processed);
}
