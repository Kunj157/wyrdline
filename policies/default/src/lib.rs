//! Wyrdline default policy (plan §17.3): writes stay in the workspace;
//! reading SSH, cloud, GitHub CLI, or GnuPG credentials or `.env` files
//! asks; egress goes to package registries, GitHub, and the model API, and
//! `curl`/`wget` anywhere else asks; force-pushes and `rm -rf /`-shaped
//! commands are denied. The rules live in `policy-common`.
#![allow(clippy::too_many_arguments)] // generated bindings

policy_common::policy_component!(policy_common::Profile::Default);
