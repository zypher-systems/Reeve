# Build from source, offline, with the vendored crates each release ships
# (reeve-<version>-vendor.tar.xz). Works with rpmbuild, mock, and COPR.
#
#   spectool -g -R packaging/fedora/reeve.spec && rpmbuild -ba packaging/fedora/reeve.spec
#
# The release also ships a prebuilt RPM (static binary) for quick installs.

# Rust attributes (`#![deny(...)]`) in vendored sources look like shebangs
# to the checker that scans the debug sources.
%global __brp_mangle_shebangs_exclude_from ^/usr/src/debug/.*$

Name:           reeve
Version:        0.4.0
Release:        1%{?dist}
Summary:        An operator agent that manages this computer, with receipts

License:        Apache-2.0
URL:            https://github.com/zypher-systems/reeve
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
Source1:        %{url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  gcc
BuildRequires:  systemd-rpm-macros

Requires:       bash
Requires:       sudo
Requires:       util-linux
Requires:       systemd
Recommends:     libnotify
Recommends:     snapper

%description
Reeve is an operator harness: an agent that runs on your computer and
manages it. It installs and removes packages, fixes services, reads logs,
tidies disks, and edits configuration across the whole filesystem. Every
action is classified by risk and approved by you where it should be, and
each one leaves a hash-chained receipt. File, package, and service changes
can be undone. The reeved user service watches the machine, learns what
normal looks like, and tells you when something needs a look.

%prep
%autosetup -n %{name}-%{version}
tar -xJf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'CARGO'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
CARGO
# rust-toolchain.toml pins the developers' toolchain; build with Fedora's.
rm -f rust-toolchain.toml

%build
cargo build --release --locked --offline -p reeve-cli

%install
install -Dpm 0755 target/release/reeve %{buildroot}%{_bindir}/reeve
install -Dpm 0644 packaging/systemd/reeved.service %{buildroot}%{_userunitdir}/reeved.service

%check
%{buildroot}%{_bindir}/reeve --version

%post
%systemd_user_post reeved.service

%preun
%systemd_user_preun reeved.service

%files
%license LICENSE
%doc README.md config.example.toml DECISIONS.md design.md
%{_bindir}/reeve
%{_userunitdir}/reeved.service

%changelog
* Tue Sep 29 2026 Zypher Systems <zypher@zyphersystems.com> - 0.4.0-1
- Standing orders by asking: Reeve writes the order and asks once, in plain words; nothing answers for the owner.
- A form for orders (n in F7), with help under every field and a live summary of what it will do.
- Nothing done to an order is lost: saves keep comments and edits made meanwhile, and every change can be undone.
- Limits follow the schedule, so every 30m runs every 30 minutes.

* Tue Sep 29 2026 Zypher Systems <zypher@zyphersystems.com> - 0.3.2-1
- 0.3.1's changes, published; a verified change's checks refuse writes that don't need a yes.

* Tue Sep 29 2026 Zypher Systems <zypher@zyphersystems.com> - 0.3.1-1
- Asks less: reads (awk and sed that print, shell loops, builtins, --help) run at once; a scratch folder never asks.
- On an approval, a is yes to the rest of the request and s allows that kind of change for the session.
- [approvals] undoable (off by default) runs changes Reeve can undo without asking.

* Tue Sep 29 2026 Zypher Systems <zypher@zyphersystems.com> - 0.3.0-1
- A new TUI: a board of eight tiles (F1-F8); an open tile keeps the others live in a strip along the top.
- New screens: needs you, activity, and what changed. The default theme is slate.

* Mon Sep 28 2026 Zypher Systems <zypher@zyphersystems.com> - 0.2.0-1
- A new TUI: the ledger (one timeline of what Reeve did and what it cost), full-screen tabs, and ctrl+k search.
- New spend and system screens; the default theme is ink.

* Mon Sep 28 2026 Zypher Systems <zypher@zyphersystems.com> - 0.1.4-1
- reeve report and /report: the state of the machine as a page, with what changed.
- The model list hides models Reeve can't use.

* Sun Sep 27 2026 Zypher Systems <zypher@zyphersystems.com> - 0.1.3-1
- Verified changes: fixes state their checks up front and roll back if they fail.
- Privacy: secrets and identifying details are masked before anything leaves; OpenRouter no-training routing.

* Sun Sep 27 2026 Zypher Systems <zypher@zyphersystems.com> - 0.1.2-1
- The observer reports quietly; popups only when a proposed fix is ready.

* Sun Sep 27 2026 Zypher Systems <zypher@zyphersystems.com> - 0.1.1-1
- Standing orders; one finding per crashing program; paced notifications;
  the agent sees reeved's findings.

* Sun Sep 27 2026 Zypher Systems <zypher@zyphersystems.com> - 0.1.0-1
- First package: the TUI, tools, receipts, memory, and the reeved observer.
