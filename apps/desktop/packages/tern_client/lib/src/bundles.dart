import 'models.dart';

/// An installed bundle (tooling spec §6.5): where it came from, pinned to
/// a hash and, when signed, a key.
class InstalledBundle {
  const InstalledBundle({
    required this.name,
    required this.version,
    required this.source,
    required this.sha256,
    this.key,
    required this.signature,
    this.keyTrusted = false,
    this.agent,
    required this.installedAt,
    this.connectors = const [],
    this.skills = const [],
    this.routines = const [],
  });

  factory InstalledBundle.fromJson(Json j) => InstalledBundle(
    name: j['name'] as String,
    version: j['version'] as String,
    source: j['source'] as String,
    sha256: j['sha256'] as String,
    key: j['key'] as String?,
    signature: j['signature'] as String? ?? '',
    keyTrusted: j['key_trusted'] as bool? ?? false,
    agent: j['agent'] as String?,
    installedAt: j['installed_at'] as String? ?? '',
    connectors: _strings(j['connectors']),
    skills: _strings(j['skills']),
    routines: [
      for (final r in j['routines'] as List? ?? const [])
        (r as Json)['name'] as String,
    ],
  );

  final String name;
  final String version;

  /// A URL or path; `openclaw:<dir>` for an import.
  final String source;
  final String sha256;

  /// The signing key, or null when unsigned.
  final String? key;

  /// `signed by D4CA… (trusted)`, `unsigned`.
  final String signature;
  final bool keyTrusted;
  final String? agent;
  final String installedAt;
  final List<String> connectors;
  final List<String> skills;
  final List<String> routines;

  bool get signed => key != null;

  /// Made by `tern import openclaw`, so not fetched again by an update.
  bool get imported => source.startsWith('openclaw:');
}

/// `POST /v1/bundles/preview`: the plan, and whether it can go in.
class BundlePreview {
  const BundlePreview({
    required this.plan,
    this.conflicts = const [],
    required this.grows,
    required this.canInstall,
  });

  factory BundlePreview.fromJson(Json j) => BundlePreview(
    plan: BundlePlan.fromJson(j['plan'] as Json),
    conflicts: _strings(j['conflicts']),
    grows: j['grows'] as bool? ?? true,
    canInstall: j['can_install'] as bool? ?? false,
  );

  final BundlePlan plan;

  /// Names taken by your own things: it can't install until they're gone.
  final List<String> conflicts;

  /// A first install, or an update that can do more: it needs approving.
  final bool grows;
  final bool canInstall;
}

/// Everything installing a bundle would do.
class BundlePlan {
  const BundlePlan({
    required this.name,
    required this.version,
    this.description = '',
    this.author = '',
    required this.source,
    required this.sha256,
    required this.signatureState,
    this.keyId,
    this.trustedAs,
    this.connectors = const [],
    this.skills = const [],
    this.routines = const [],
    this.secrets = const [],
    this.connections = const [],
    this.warnings = const [],
    this.readme,
    this.installedVersion,
    this.installedSha256,
    this.added = const [],
    this.removed = const [],
    this.changed = const [],
  });

  factory BundlePlan.fromJson(Json j) {
    final sig = j['signature'] as Json? ?? const {};
    final changes = j['changes'] as Json?;
    return BundlePlan(
      name: j['name'] as String,
      version: j['version'] as String,
      description: j['description'] as String? ?? '',
      author: j['author'] as String? ?? '',
      source: j['source'] as String? ?? '',
      sha256: j['sha256'] as String,
      signatureState: sig['state'] as String? ?? 'unsigned',
      keyId: sig['key_id'] as String?,
      trustedAs: sig['trusted_as'] as String?,
      connectors: [
        for (final c in j['connectors'] as List? ?? const [])
          BundleConnector.fromJson(c as Json),
      ],
      skills: [
        for (final s in j['skills'] as List? ?? const [])
          BundleSkill.fromJson(s as Json),
      ],
      routines: [
        for (final r in j['routines'] as List? ?? const [])
          BundleRoutine.fromJson(r as Json),
      ],
      secrets: [
        for (final s in j['secrets'] as List? ?? const [])
          (
            name: (s as Json)['name'] as String,
            usedBy: _strings(s['used_by']),
            set: s['set'] as bool? ?? false,
          ),
      ],
      connections: [
        for (final c in j['connections'] as List? ?? const [])
          (
            connector: (c as Json)['connector'] as String,
            provider: c['provider'] as String,
            scopes: _strings(c['scopes']),
            connected: c['connected'] as bool? ?? false,
          ),
      ],
      warnings: _strings(j['warnings']),
      readme: j['readme'] as String?,
      installedVersion: (j['installed'] as Json?)?['version'] as String?,
      installedSha256: (j['installed'] as Json?)?['sha256'] as String?,
      added: _strings(changes?['added']),
      removed: _strings(changes?['removed']),
      changed: _strings(changes?['changed']),
    );
  }

  final String name;
  final String version;
  final String description;
  final String author;
  final String source;

  /// What pins it; installing sends it back.
  final String sha256;

  /// `unsigned`, `trusted`, `pinned`, `unknown` or `new_key`.
  final String signatureState;
  final String? keyId;
  final String? trustedAs;
  final List<BundleConnector> connectors;
  final List<BundleSkill> skills;
  final List<BundleRoutine> routines;
  final List<({String name, List<String> usedBy, bool set})> secrets;
  final List<
    ({String connector, String provider, List<String> scopes, bool connected})
  >
  connections;
  final List<String> warnings;
  final String? readme;

  /// Set when this replaces an installed version.
  final String? installedVersion;
  final String? installedSha256;
  final List<String> added;
  final List<String> removed;
  final List<String> changed;

  bool get isUpdate => installedVersion != null;

  /// The installed version is exactly this one.
  bool get upToDate => isUpdate && installedSha256 == sha256;

  /// The signature in words.
  String get signatureWords => switch (signatureState) {
    'unsigned' => 'Unsigned: nothing shows who made it',
    'trusted' =>
      'Signed by $keyId (trusted${trustedAs == null ? '' : ': $trustedAs'})',
    'pinned' => 'Signed by $keyId, the key of the installed version',
    'new_key' => 'Signed by a NEW key, $keyId',
    _ => 'Signed by $keyId, a key you haven\'t trusted',
  };
}

class BundleConnector {
  const BundleConnector({
    required this.name,
    this.description = '',
    this.hosts = const [],
    this.auth = const [],
    this.oauth,
    this.rawSecrets = const [],
    required this.rate,
    this.tools = const [],
    this.warnings = const [],
    this.existing,
  });

  factory BundleConnector.fromJson(Json j) => BundleConnector(
    name: j['name'] as String,
    description: j['description'] as String? ?? '',
    hosts: _strings(j['hosts']),
    auth: _strings(j['auth']),
    oauth: j['oauth'] as String?,
    rawSecrets: _strings(j['raw_secrets']),
    rate: j['rate'] as String? ?? '',
    tools: [
      for (final t in j['tools'] as List? ?? const [])
        (
          name: (t as Json)['name'] as String,
          description: t['description'] as String? ?? '',
          effect: t['effect'] as String? ?? 'write',
          flags: _strings(t['flags']),
        ),
    ],
    warnings: _strings(j['warnings']),
    existing: j['existing'] as String?,
  );

  final String name;
  final String description;

  /// Hosts, optionally with paths, it may reach.
  final List<String> hosts;

  /// How tern sends credentials (the script never sees them).
  final List<String> auth;
  final String? oauth;

  /// Secrets it reads itself (legacy).
  final List<String> rawSecrets;
  final String rate;
  final List<
    ({String name, String description, String effect, List<String> flags})
  >
  tools;
  final List<String> warnings;
  final String? existing;

  bool get hasLookalike => warnings.any((w) => w.startsWith('lookalike'));
}

class BundleSkill {
  const BundleSkill({
    required this.slug,
    required this.name,
    this.description = '',
    this.files = const [],
    this.runnable = const [],
  });

  factory BundleSkill.fromJson(Json j) => BundleSkill(
    slug: j['slug'] as String,
    name: j['name'] as String,
    description: j['description'] as String? ?? '',
    files: _strings(j['files']),
    runnable: _strings(j['runnable']),
  );

  final String slug;
  final String name;
  final String description;
  final List<String> files;

  /// Files the agent could run, not only read.
  final List<String> runnable;
}

class BundleRoutine {
  const BundleRoutine({
    required this.name,
    required this.trigger,
    required this.prompt,
    required this.initiative,
    required this.autonomy,
    this.limits = const [],
    this.notes = const [],
  });

  factory BundleRoutine.fromJson(Json j) => BundleRoutine(
    name: j['name'] as String,
    trigger: j['trigger'] as String,
    prompt: j['prompt'] as String? ?? '',
    initiative: j['initiative'] as String? ?? '',
    autonomy: j['autonomy'] as String? ?? '',
    limits: _strings(j['limits']),
    notes: _strings(j['notes']),
  );

  final String name;

  /// `cron 0 8 * * 1-5`, `once at …`, `watch x.y every 5m`.
  final String trigger;
  final String prompt;
  final String initiative;
  final String autonomy;
  final List<String> limits;
  final List<String> notes;
}

/// What an install or removal did, line by line.
class BundleOutcome {
  const BundleOutcome({required this.name, this.lines = const []});

  factory BundleOutcome.fromJson(Json j) =>
      BundleOutcome(name: j['name'] as String, lines: _strings(j['lines']));

  final String name;
  final List<String> lines;
}

List<String> _strings(Object? v) => [
  for (final s in v as List? ?? const []) s as String,
];
