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
enum DepType {
    Dev,
    Normal,
}

#[derive(Debug, Clone)]
struct Dep {
    name: String,
    version: String,
    r#type: DepType,
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
    let mut dep_versions = package
        .time
        .keys()
        .filter(|version| !version.contains("-") && version.contains("."))
        .map(|version| Version::parse(version).expect("cannot parse version"));

    let requested_version =
        VersionReq::parse(&dep.version).expect("cannot parse requested version");

    let resolved_version: Version;

    if let Some(version) = dep_versions.find(|version| requested_version.matches(version)) {
        resolved_version = version;
    } else {
        let versions: Vec<Version> = dep_versions.collect();

        if versions.len() == 1 {
            resolved_version = versions.get(0).expect("there is no versions available").clone();
        } else {
            resolved_version = versions
                .iter()
                .max()
                .unwrap_or(versions.get(0).expect("there is no versions available"))
                .clone();
        }
    }

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

// fn mark_deps()

#[async_recursion]
async fn process_deps(queue: Deps, processed_deps: ProcessedDeps) {
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = vec![];

    loop {
        let queue = queue.clone();
        let processed_deps = processed_deps.clone();

        let dep = { queue.lock().unwrap().pop() };

        if let Some(dep) = dep {
            tasks.push(tokio::spawn(async move {
                logger::log_fetching(&dep.name);
                let fetched_dep = fetch_dep(&dep).await;

                {
                    let mut processed_deps = processed_deps.lock().unwrap();

                    processed_deps.insert(dep.name.clone(), dep);
                }

                if let Some(normal_deps) = fetched_dep.dependencies {
                    let processed_deps = processed_deps.lock().unwrap();

                    for (k, v) in normal_deps.iter() {
                        if !processed_deps.contains_key(k) {
                            let dep = Dep {
                                name: k.clone(),
                                version: v.clone(),
                                r#type: DepType::Normal,
                            };

                            queue.lock().unwrap().push(dep);
                        }
                    }
                }
            }))
        } else {
            break;
        }
    }

    join_all(tasks).await;

    let queue_len = queue.lock().unwrap().len();

    if queue_len > 0 {
        process_deps(queue, processed_deps).await;
    }
}

#[tokio::main]
async fn main() {
    let package = parse_root_package();

    let queue: Deps = Arc::new(Mutex::new(Vec::new()));
    let processed_deps: ProcessedDeps = Arc::new(Mutex::new(HashMap::new()));

    if let Some(normal_deps) = package.dependencies {
        let mut queue = queue.lock().unwrap();

        normal_deps.into_iter().for_each(|(name, version)| {
            let dep = Dep {
                name,
                version,
                r#type: DepType::Normal,
            };

            queue.push(dep);
        });
    }

    if let Some(dev_deps) = package.dev_dependencies {
        let mut queue = queue.lock().unwrap();

        dev_deps.into_iter().for_each(|(name, version)| {
            let dep = Dep {
                name,
                version,
                r#type: DepType::Dev,
            };

            queue.push(dep);
        });
    }

    process_deps(queue, processed_deps.clone()).await;

    let processed = processed_deps.lock().unwrap();

    println!("Fetched {} packages", processed.len())

    // for (_, dep) in processed.iter() {
    //     println!("fetched {}: {}", dep.name, dep.version);
    // }
}
