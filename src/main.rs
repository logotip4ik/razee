use async_recursion::async_recursion;
use futures::future::join_all;
use reqwest::{Client, StatusCode};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, env, fs, io::BufReader};

mod logger;

type DependenciesMap = HashMap<String, String>;
type Deps = Arc<Mutex<Vec<Dep>>>;
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

fn resolve_version(package: &RegistryPackage, requested_version: &VersionReq) -> Version {
    let mut dep_versions = package
        .time
        .keys()
        .filter(|version| !version.contains("-") && version.contains("."))
        .map(|version| Version::parse(version).expect("cannot parse version"));

    if let Some(version) = dep_versions.find(|version| requested_version.matches(version)) {
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

    let url = REGISTRY_URL.to_owned() + "/" + dep.name.as_str();
    let package = client
        .get(url)
        .header("User-Agent", "Razee (Node Package Manger in Rust)")
        .send()
        .await
        .expect("probably no internet");

    assert_eq!(package.status(), StatusCode::OK);

    let body = package.text().await.expect("cannot read package deps");
    let package: RegistryPackage = serde_json::from_str(body.as_str())
        .expect(format!("cannot parse dependency {} on {}\n", dep.name, dep.version).as_str());

    let requested_version =
        VersionReq::parse(&dep.version).expect("cannot parse requested version");

    let resolved_version = resolve_version(&package, &requested_version);

    let dep_url: String = format!("{}/{}/{}", REGISTRY_URL, dep.name, resolved_version);
    let res = client
        .get(dep_url)
        .header("User-Agent", "Razee (Node Package Manger in Rust)")
        .send()
        .await
        .expect("probably no internet");

    assert_eq!(res.status(), StatusCode::OK);

    let body = res.text().await.expect("cannot read dependency body");

    let dependency = serde_json::from_str(body.as_str())
        .expect(format!("cannot parse dependency {} on {}\n", dep.name, dep.version).as_str());

    return dependency;
}

#[async_recursion]
async fn process_dep(dep: Dep, processed_deps: ProcessedDeps) {
    let package = fetch_dep(&dep).await;

    {
        let mut processed = processed_deps.lock().unwrap();

        logger::log_processed(&dep.name);

        processed.insert(dep.name.clone(), dep);
    }

    let mut needs_processing = vec![];

    if let Some(deps) = package.dependencies {
        let processed = processed_deps.lock().unwrap();

        for (k, v) in deps.iter() {
            if !processed.contains_key(k) {
                needs_processing.push(Dep {
                    name: k.to_owned(),
                    version: v.to_owned(),
                });
            }
        }
    }

    join_all(needs_processing.iter().map(|dep| {
        let dep = dep.to_owned();
        let processed_deps = processed_deps.clone();

        tokio::spawn(async move {
            process_dep(dep, processed_deps).await;
        })
    }))
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

    join_all(needs_processing.iter().map(|dep| {
        let dep = dep.to_owned();
        let processed_deps = processed_deps.clone();

        tokio::spawn(async move {
            process_dep(dep, processed_deps).await;
        })
    }))
    .await;

    // process_deps(queue, processed_deps.clone()).await;

    let processed = processed_deps.lock().unwrap();

    println!("Fetched {} packages", processed.len())
}
