use async_recursion::async_recursion;
use flate2::read::GzDecoder;
use futures::future::join_all;
use node_semver::{Range, Version};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::{
    collections::HashMap,
    env, fs,
    io::{BufReader, Cursor},
    path::Path,
};
use tar::Archive;
use tokio::sync::Mutex;

mod logger;

type DependenciesMap = HashMap<String, String>;
type ProcessedDeps = Arc<Mutex<HashMap<String, Dep>>>;

#[derive(Debug, Serialize, Deserialize)]
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
    let mut dep_versions = package
        .time
        .keys()
        .filter(|version| !version.contains("-") && version.contains("."))
        .map(|version| Version::parse(version).expect("cannot parse version"));

    if let Some(version) = dep_versions.find(|version| requested_version.satisfies(version)) {
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
                .unwrap_or(versions.get(0).expect("there is no versions available"))
                .clone();
        }
    }
}

async fn fetch_dep(dep: &Dep) -> Dependency {
    let client = Client::new();

    let url: String = format!("{REGISTRY_URL}/{}", dep.name);
    // TODO: use json feature of reqwest
    let package: RegistryPackage = client
        .get(&url)
        .header("User-Agent", "Razee (Node Package Manger in Rust)")
        .send()
        .await
        .expect("probably no internet")
        .json()
        .await
        .expect("cannot parse dependency");

    let requested_version = Range::parse(&dep.version).expect(
        format!(
            "cannot parse requested version: {}:{}",
            package.name, dep.version
        )
        .as_str(),
    );

    let resolved_version = resolve_version(&package, &requested_version);

    let url: String = format!("{REGISTRY_URL}/{}/{resolved_version}", dep.name);
    let dependency: Dependency = client
        .get(&url)
        .header("User-Agent", "Razee (Node Package Manger in Rust)")
        .send()
        .await
        .expect("probably no internet")
        .json()
        .await
        .unwrap();

    return dependency;
}

async fn fetch_tarball(dep_name: &String, dep_version: &String, dep_tarball: &String) {
    let dep_dir = format!("{NODE_MODULES}/{dep_name}");
    let dep_dir_package = format!("{dep_dir}/package.json");

    if Path::new(&dep_dir_package).exists() {
        return;
    }

    let client = Client::new();

    let tarball_bytes = client
        .get(dep_tarball)
        .header("User-Agent", "Razee (Node Package Manger in Rust)")
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();

    let tarball_cursor = Cursor::new(tarball_bytes);
    let tarball = GzDecoder::new(tarball_cursor);

    let mut archive = Archive::new(tarball);

    for entry in archive.entries().unwrap() {
        if let Ok(mut entry) = entry {
            let path = entry
                .path()
                .unwrap()
                .to_str()
                .unwrap()
                .replace("package", &dep_dir)
                .to_owned();

            let mut folders: Vec<&str> = path.split("/").collect();
            folders.pop();

            if folders.len() > 1 {
                fs::create_dir_all(folders.join("/")).unwrap();
            }

            entry.unpack(&path).unwrap();
        }
    }
}

#[async_recursion]
async fn process_dep(dep: &Dep, processed_deps: ProcessedDeps) {
    let package = fetch_dep(&dep).await;
    let tarball_promise = fetch_tarball(&package.name, &package.version, &package.dist.tarball);

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
            .map(|dep| process_dep(dep, processed_deps.clone()))
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

    join_all(
        needs_processing
            .iter()
            .map(|dep| process_dep(dep, processed_deps.clone()))
            .collect::<Vec<_>>(),
    )
    .await;

    let processed = processed_deps.lock().await;

    println!("Fetched {} packages", processed.len());
    println!("{:?}", processed);
}
