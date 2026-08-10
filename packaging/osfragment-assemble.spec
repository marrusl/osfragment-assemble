%global debug_package %{nil}

Name:           osfragment-assemble
Version:        0.1.0
Release:        1%{?dist}
Summary:        Composable image definitions for bootc-compatible OS images

License:        MIT
URL:            https://github.com/marrusl/osfragment-assemble
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust

%description
osfragment-assemble generates Containerfiles from composable fragment
images for bootc-compatible OS images. Define reusable fragments as OCI
images, compose them via a YAML manifest, and produce a ready-to-build
Containerfile.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release

%install
install -Dpm 0755 target/release/osfragment-assemble %{buildroot}%{_bindir}/osfragment-assemble

%files
%license LICENSE
%doc README.md
%{_bindir}/osfragment-assemble

%changelog
%autochangelog
