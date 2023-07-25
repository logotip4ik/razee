use bytes::Bytes;
use elsa::FrozenMap;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};

use crate::{Dependency, DependencyDist, RegistryPackage, Dep};

const REGISTRY_URL: &str = "http://registry.npmjs.org";

pub struct HttpClient {
    client: ClientWithMiddleware,
    tarball_cache: FrozenMap<String, Box<Bytes>>,
    package_cache: FrozenMap<String, Box<RegistryPackage>>,
    dependency_cache: FrozenMap<String, Box<Dependency>>,
}

impl HttpClient {
    pub fn new() -> HttpClient {
        let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);
        let client = ClientBuilder::new(reqwest::Client::new())
            .with(RetryTransientMiddleware::new_with_policy(retry_policy))
            .build();

        // let client = reqwest::Client::new();
        
        return HttpClient {
            client,
            tarball_cache: FrozenMap::new(),
            package_cache: FrozenMap::new(),
            dependency_cache: FrozenMap::new(),
        };
    }

    /// fetches specific package version for gathering tarball url and other dependencies
    pub async fn fetch_dependency(&self, url: &String) -> &Dependency {
        if let Some(dependency) = self.dependency_cache.get(url) {
            return dependency;
        }

        let dependency: Dependency = self
            .client
            .get(url)
            .header("User-Agent", "Razee (Node Package Manger in Rust)")
            .send()
            .await
            .expect("probably no internet")
            .json()
            .await
            .unwrap();

        return self
            .dependency_cache
            .insert(url.to_string(), Box::new(dependency));
    }

    /// fetches package info to resolve version
    pub async fn fetch_package(&self, dep: &Dep) -> &RegistryPackage {
        let url = format!("{REGISTRY_URL}/{}", dep.name);
        
        if let Some(package) = self.package_cache.get(&url) {
            return package;
        }

        let package: RegistryPackage = self
            .client
            .get(&url)
            .header("User-Agent", "Razee (Node Package Manger in Rust)")
            .send()
            .await
            .expect("probably no internet")
            .json::<RegistryPackage>()
            .await
            .expect("cannot parse dependency");

        return self
            .package_cache
            .insert(url.to_string(), Box::new(package));
    }

    /// fetches tarball for package
    pub async fn fetch_tarball(&self, dist: &DependencyDist) -> &Bytes {
        if let Some(tarball) = self.tarball_cache.get(&dist.tarball) {
            return tarball;
        }

        let tarball = self
            .client
            .get(&dist.tarball)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();

        return self
            .tarball_cache
            .insert(dist.tarball.to_string(), Box::new(tarball));
    }
}
