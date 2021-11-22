//! Configures the given network namespace with provided specs
use crate::error::NetavarkError;
use crate::firewall;
use crate::firewall::iptables::MAX_HASH_SIZE;
use crate::network;
use crate::network::types;
use clap::{self, Clap};
use log::debug;
use std::collections::HashMap;
use std::error::Error;
use sysctl::Sysctl;
use sysctl::SysctlError;

const IPV4_FORWARD: &str = "net.ipv4.ip_forward";

#[derive(Clap, Debug)]
pub struct Setup {
    /// Network namespace path
    #[clap(forbid_empty_values = true, required = true)]
    network_namespace_path: String,
}

impl Setup {
    /// The setup command configures the given network namespace with the given configuration, creating any interfaces and firewall rules necessary.
    pub fn new(network_namespace_path: String) -> Self {
        Self {
            network_namespace_path,
        }
    }

    pub fn exec(&self, input_file: String) -> Result<(), Box<dyn Error>> {
        match network::validation::ns_checks(&self.network_namespace_path) {
            Ok(_) => (),
            Err(e) => {
                bail!("invalid namespace path: {}", e);
            }
        }

        debug!("{:?}", "Setting up...");

        let network_options = match network::types::NetworkOptions::load(&input_file) {
            Ok(opts) => opts,
            Err(e) => {
                return Err(Box::new(NetavarkError {
                    error: format!("{}", e),
                    errno: 1,
                }));
            }
        };

        let firewall_driver = match firewall::get_supported_firewall_driver() {
            Ok(driver) => driver,
            Err(e) => panic!("{}", e.to_string()),
        };

        // Sysctl setup
        // set ip forwarding to 1 if not already
        let sysctl_ipv4 = get_sysctl_value(IPV4_FORWARD)?;
        if sysctl_ipv4 != *"1" {
            set_sysctl_value(IPV4_FORWARD, "1")?;
        }

        let mut response: HashMap<String, types::StatusBlock> = HashMap::new();

        // Perform per-network setup
        for (net_name, network) in network_options.network_info.iter() {
            debug!(
                "Setting up network {} with driver {}",
                net_name, network.driver
            );

            match network.driver.as_str() {
                "bridge" => {
                    let per_network_opts =
                        network_options.networks.get(net_name).ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("network options for network {} not found", net_name),
                            )
                        })?;
                    //Configure Bridge and veth_pairs
                    let status_block = network::core::Core::bridge_per_podman_network(
                        per_network_opts,
                        network,
                        &self.network_namespace_path,
                    )?;
                    response.insert(net_name.to_owned(), status_block);

                    let id_network_hash = network::core_utils::CoreUtils::create_network_hash(
                        net_name,
                        MAX_HASH_SIZE,
                    );

                    firewall_driver.setup_network(network.clone(), id_network_hash.clone())?;
                    let port_bindings = network_options.port_mappings.clone();
                    match port_bindings {
                        None => {}
                        Some(i) => {
                            // Need to enable sysctl localnet so that traffic can pass
                            // through localhost to containers
                            let network_interface = &network.network_interface;
                            match network_interface {
                                None => {}
                                Some(i) => {
                                    let localnet_path =
                                        format!("net.ipv4.conf.{}.route_localnet", i);
                                    let sysctl_localnet = get_sysctl_value(localnet_path.as_str())?;
                                    if sysctl_localnet != *"1" {
                                        set_sysctl_value(localnet_path.as_str(), "1")?;
                                    }
                                }
                            }
                            let container_ips =
                                &per_network_opts.static_ips.as_ref().ok_or_else(|| {
                                    std::io::Error::new(
                                        std::io::ErrorKind::Other,
                                        "no container ip provided",
                                    )
                                })?;
                            let networks = &network.subnets.as_ref().ok_or_else(|| {
                                std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    "no network address provided",
                                )
                            })?;
                            firewall_driver.setup_port_forward(
                                &network_options.container_id,
                                i,
                                container_ips[0],
                                networks[0].subnet,
                                net_name,
                                &id_network_hash.as_str()[0..MAX_HASH_SIZE],
                            )?;
                        }
                    }
                }
                "macvlan" => {
                    let per_network_opts =
                        network_options.networks.get(net_name).ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("network options for network {} not found", net_name),
                            )
                        })?;
                    //Configure Bridge and veth_pairs
                    let status_block = network::core::Core::macvlan_per_podman_network(
                        per_network_opts,
                        network,
                        &self.network_namespace_path,
                    )?;
                    response.insert(net_name.to_owned(), status_block);
                }
                // unknown driver
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("unknown network driver {}", network.driver),
                    )
                    .into());
                }
            }
        }

        debug!("{:#?}", response);
        let response_json = serde_json::to_string(&response)?;
        println!("{}", response_json);
        debug!("{:?}", "Setup complete");
        Ok(())
    }
}
// get a sysctl value by the value's namespace
fn get_sysctl_value(ns_value: &str) -> Result<String, SysctlError> {
    debug!("Getting sysctl value for {}", ns_value);
    let ctl = sysctl::Ctl::new(ns_value)?;
    ctl.value_string()
}

// set a sysctl value by value's namespace
fn set_sysctl_value(ns_value: &str, val: &str) -> Result<String, SysctlError> {
    debug!("Setting sysctl value for {} to {}", ns_value, val);
    let ctl = sysctl::Ctl::new(ns_value)?;
    ctl.set_value_string(val)
}
