/// The desktop app's own version (the core's comes from `/v1/health`).
///
/// The build scripts pass the workspace version (`scripts/release-macos.sh`,
/// `scripts/package-linux-app.sh`). A plain `flutter run` has none, so it is
/// empty and nothing compares against it.
const appVersion = String.fromEnvironment('TERN_APP_VERSION');

/// How the version reads in the UI.
String appVersionLabel([String version = appVersion]) =>
    version.isEmpty ? 'development build' : version;

/// What to tell the user when the core they reached is not the version this
/// app was built with, or null when they match (or there is nothing to
/// compare: a development build, or a core on a server, which is not ours to
/// restart).
String? coreSkewMessage({
  required String appVersion,
  required String coreVersion,
  required bool local,
}) {
  if (!local || appVersion.isEmpty || coreVersion == appVersion) return null;
  return 'This app is v$appVersion but the tern running here is '
      'v$coreVersion. Restart tern to use the version that came with the '
      'app; until then some features may be missing or behave differently.';
}
