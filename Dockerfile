# The standalone CleverHans agent service (spec §10.2).
#
# Built by the release workflow from prebuilt static (musl) binaries laid
# out as binaries/<amd64|arm64>/cleverhans — see .github/workflows/release.yml.
# Local build: scripts/set-version.sh docs cover the release path; for a
# one-off local image, cross-compile with
#   cargo build --release -p cleverhans-serve --target x86_64-unknown-linux-musl
# and arrange the binary under binaries/amd64/.
FROM gcr.io/distroless/static-debian12:nonroot
ARG TARGETARCH
COPY binaries/${TARGETARCH}/cleverhans /usr/local/bin/cleverhans
ENTRYPOINT ["/usr/local/bin/cleverhans"]
# Mount your registry + config at /etc/cleverhans (override CMD as needed).
CMD ["serve", "--registry", "/etc/cleverhans/registry.json", "--config", "/etc/cleverhans/cleverhans.toml", "--bind", "0.0.0.0:8789"]
