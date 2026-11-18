import 'models.dart';

/// What "Install for me" would do (`GET /v1/local/status`, `install`).
class LocalInstallPlan {
  const LocalInstallPlan({
    this.source = '',
    this.destination = '',
    this.summary = '',
    this.available = false,
  });

  factory LocalInstallPlan.fromJson(Json j) => LocalInstallPlan(
    source: j['source'] as String? ?? '',
    destination: j['destination'] as String? ?? '',
    summary: j['summary'] as String? ?? '',
    available: j['available'] as bool? ?? false,
  );

  final String source;
  final String destination;

  /// A sentence saying what it does: shown before the person agrees.
  final String summary;

  /// Possible on this computer.
  final bool available;
}

/// How far "Install for me" has got: `idle`, `working`, `done`, `failed`.
class LocalInstalling {
  const LocalInstalling({
    this.state = 'idle',
    this.phase,
    this.completed = 0,
    this.total = 0,
    this.signer,
    this.message,
  });

  factory LocalInstalling.fromJson(Json j) => LocalInstalling(
    state: j['state'] as String? ?? 'idle',
    phase: j['phase'] as String?,
    completed: (j['completed'] as num?)?.toInt() ?? 0,
    total: (j['total'] as num?)?.toInt() ?? 0,
    signer: j['signer'] as String?,
    message: j['message'] as String?,
  );

  final String state;

  /// While working: `fetching`, `downloading`, `verifying`, `installing`.
  final String? phase;
  final int completed;
  final int total;
  final String? signer;
  final String? message;

  bool get working => state == 'working';
  bool get failed => state == 'failed';

  /// 0..1 while downloading, else null (no bar, just a spinner).
  double? get fraction =>
      phase == 'downloading' && total > 0 ? completed / total : null;
}

/// The model first run offers, sized to this Mac.
class LocalStarter {
  const LocalStarter({required this.model, this.sizeGb = 0, this.blurb = ''});

  factory LocalStarter.fromJson(Json j) => LocalStarter(
    model: j['model'] as String,
    sizeGb: (j['size_gb'] as num?)?.toDouble() ?? 0,
    blurb: j['blurb'] as String? ?? '',
  );

  final String model;
  final double sizeGb;
  final String blurb;
}

/// The local engine (Ollama): `GET /v1/local/status`.
class LocalStatus {
  const LocalStatus({
    this.engine = 'ollama',
    this.installed = false,
    this.running = false,
    this.version,
    this.tooOld = false,
    this.managed = false,
    this.url = '',
    this.modelsDir,
    this.diskFree,
    this.ramBytes,
    this.context,
    this.warnings = const [],
    this.installPage = 'https://ollama.com/download',
    this.install = const LocalInstallPlan(),
    this.installing = const LocalInstalling(),
    this.starter,
  });

  factory LocalStatus.fromJson(Json j) => LocalStatus(
    engine: j['engine'] as String? ?? 'ollama',
    installed: j['installed'] as bool? ?? false,
    running: j['running'] as bool? ?? false,
    version: j['version'] as String?,
    tooOld: j['too_old'] as bool? ?? false,
    managed: j['managed'] as bool? ?? false,
    url: j['url'] as String? ?? '',
    modelsDir: j['models_dir'] as String?,
    diskFree: (j['disk_free'] as num?)?.toInt(),
    ramBytes: (j['ram_bytes'] as num?)?.toInt(),
    context: (j['context'] as num?)?.toInt(),
    warnings: [for (final w in j['warnings'] as List? ?? []) w as String],
    installPage: j['install_page'] as String? ?? 'https://ollama.com/download',
    install: j['install'] is Json
        ? LocalInstallPlan.fromJson(j['install'] as Json)
        : const LocalInstallPlan(),
    installing: j['installing'] is Json
        ? LocalInstalling.fromJson(j['installing'] as Json)
        : const LocalInstalling(),
    starter: j['starter'] is Json
        ? LocalStarter.fromJson(j['starter'] as Json)
        : null,
  );

  final String engine;

  /// The app or command is there (it may not be running).
  final bool installed;

  /// A server answers. One the user started counts: tern reuses it.
  final bool running;
  final String? version;

  /// Older than tern can use.
  final bool tooOld;

  /// tern started the server, and stops it with the daemon.
  final bool managed;
  final String url;
  final String? modelsDir;
  final int? diskFree;
  final int? ramBytes;

  /// Tokens of context models get.
  final int? context;
  final List<String> warnings;
  final String installPage;
  final LocalInstallPlan install;
  final LocalInstalling installing;

  /// What first run downloads for this Mac.
  final LocalStarter? starter;
}

/// One installed local model (`GET /v1/local/models`).
class LocalModel {
  const LocalModel({
    required this.name,
    this.size = 0,
    this.family,
    this.parameterSize,
    this.quantization,
    this.tools,
    this.thinking,
    this.maxContext,
    this.loaded = false,
  });

  factory LocalModel.fromJson(Json j) => LocalModel(
    name: j['name'] as String,
    size: (j['size'] as num?)?.toInt() ?? 0,
    family: j['family'] as String?,
    parameterSize: j['parameter_size'] as String?,
    quantization: j['quantization'] as String?,
    tools: j['tools'] as bool?,
    thinking: j['thinking'] as bool?,
    maxContext: (j['max_context'] as num?)?.toInt(),
    loaded: j['loaded'] as bool? ?? false,
  );

  final String name;
  final int size;
  final String? family;
  final String? parameterSize;
  final String? quantization;

  /// Can call tools; null when the server didn't say.
  final bool? tools;
  final bool? thinking;
  final int? maxContext;

  /// In memory now.
  final bool loaded;
}

/// A model download (`GET /v1/local/pulls`, `local_pull` events).
class LocalPull {
  const LocalPull({
    required this.id,
    required this.model,
    required this.phase,
    this.completed = 0,
    this.total = 0,
    this.error,
  });

  factory LocalPull.fromJson(Json j) => LocalPull(
    id: j['id'] as String,
    model: j['model'] as String,
    phase: j['phase'] as String? ?? 'starting',
    completed: (j['completed'] as num?)?.toInt() ?? 0,
    total: (j['total'] as num?)?.toInt() ?? 0,
    error: j['error'] as String?,
  );

  final String id;
  final String model;

  /// `starting`, `downloading`, `verifying`, `done`, `failed`, `cancelled`.
  final String phase;
  final int completed;
  final int total;
  final String? error;

  bool get finished =>
      phase == 'done' || phase == 'failed' || phase == 'cancelled';

  /// 0..1 once the size is known.
  double? get fraction =>
      total > 0 ? (completed / total).clamp(0.0, 1.0) : null;
}
