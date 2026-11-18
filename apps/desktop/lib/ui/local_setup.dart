// First run with Ollama (docs/local-models.md): one prompt. "Set up with
// Ollama" installs it if needed, starts it (tern does that by itself, with
// no prompt), downloads a starter model sized to this Mac, and makes it the
// default. Only a step the person must take shows anything: installing
// Ollama, or agreeing to the download. Cloud providers stay one link away.
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../theme.dart';
import 'widgets.dart';

class OllamaFirstRun extends StatefulWidget {
  const OllamaFirstRun({
    super.key,
    required this.client,
    required this.onDone,
    required this.onUseApi,
  });

  final TernClient client;

  /// The starter model is the default: carry on.
  final VoidCallback onDone;

  /// "Use an API model instead": the existing provider setup.
  final VoidCallback onUseApi;

  @override
  State<OllamaFirstRun> createState() => _OllamaFirstRunState();
}

class _OllamaFirstRunState extends State<OllamaFirstRun> {
  LocalStatus? _status;
  List<LocalModel> _models = const [];
  List<LocalPull> _pulls = const [];
  String? _error;

  /// The person pressed "Set up with Ollama": carry on without asking again.
  var _go = false;
  var _starting = false;
  var _startFailed = false;
  var _pulling = false;
  var _connecting = false;
  Timer? _poll;

  @override
  void initState() {
    super.initState();
    _load();
    _poll = Timer.periodic(const Duration(milliseconds: 1500), (_) => _load());
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final s = await widget.client.localStatus();
      final pulls = await widget.client.localPulls();
      final models = s.running
          ? await widget.client.localModels()
          : const <LocalModel>[];
      if (!mounted) return;
      setState(() {
        _status = s;
        _pulls = pulls;
        _models = models;
      });
      unawaited(_advance());
    } on Object {
      // Keep what is shown; the next poll tries again.
    }
  }

  /// Models that can run tasks.
  LocalModel? get _usable => _models.where((m) => m.tools != false).firstOrNull;

  Future<void> _advance() async {
    final s = _status;
    if (s == null || !mounted) return;
    // Ollama is installed: start it, no prompt.
    if (s.installed && !s.running && !_starting && !_startFailed) {
      _starting = true;
      try {
        await widget.client.localStart();
      } on TernException catch (e) {
        if (mounted) {
          setState(() {
            _startFailed = true;
            _error = e.message;
          });
        }
      } finally {
        _starting = false;
      }
      return;
    }
    if (!s.running || s.tooOld) return;
    // A model is there: make it the default.
    if (_usable case final m? when !_connecting) {
      await _connect(m.name);
      return;
    }
    // After the person agreed, download the starter.
    final starter = s.starter;
    final active = _pulls.any((p) => !p.finished);
    if (_go && starter != null && !_pulling && !active && _usable == null) {
      _pulling = true;
      try {
        await widget.client.localPull(starter.model);
      } on TernException catch (e) {
        if (mounted) {
          setState(() {
            _go = false;
            _error = e.message;
          });
        }
      } finally {
        _pulling = false;
      }
    }
  }

  Future<void> _connect(String model) async {
    _connecting = true;
    try {
      await widget.client.saveProvider(
        const ProviderDraft(preset: 'ollama'),
        defaultModel: 'ollama:$model',
      );
      if (mounted) widget.onDone();
    } on TernException catch (e) {
      _connecting = false;
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _setUp() async {
    setState(() {
      _go = true;
      _error = null;
    });
    final s = _status;
    if (s != null && !s.installed && s.install.available) {
      try {
        await widget.client.localInstall(consent: true);
      } on TernException catch (e) {
        if (mounted) {
          setState(() {
            _go = false;
            _error = e.message;
          });
        }
      }
    }
    await _load();
  }

  void _retryStart() {
    setState(() {
      _startFailed = false;
      _error = null;
    });
    unawaited(_advance());
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = _status;
    final style = FilledButton.styleFrom(
      backgroundColor: c.accent,
      foregroundColor: c.onAccent,
    );
    Widget line(String text, {Color? color}) => Padding(
      padding: const EdgeInsets.only(top: 6),
      child: Text(
        text,
        style: TextStyle(fontSize: 12.5, color: color ?? c.muted),
      ),
    );
    final working = <Widget>[];
    Widget? action;
    if (s == null) {
      working.add(const _Spinner());
    } else if (s.installing.working) {
      working.addAll([
        LinearProgressIndicator(value: s.installing.fraction, minHeight: 3),
        line(_phase(s.installing)),
      ]);
    } else if (_startFailed) {
      working.add(line(_error ?? "Ollama didn't start.", color: c.error));
      action = TextButton(
        key: const ValueKey('local-retry'),
        onPressed: _retryStart,
        child: const Text('Try again'),
      );
    } else if (s.installed && !s.running) {
      working.addAll([const _Spinner(), line('Starting Ollama…')]);
    } else if (s.running && _pulls.any((p) => !p.finished)) {
      final p = _pulls.firstWhere((p) => !p.finished);
      working.addAll([
        LinearProgressIndicator(value: p.fraction, minHeight: 3),
        line(
          p.fraction == null
              ? 'Downloading ${p.model}…'
              : 'Downloading ${p.model} · ${(p.fraction! * 100).round()}%',
        ),
      ]);
    } else if (s.running && _usable != null) {
      working.addAll([const _Spinner(), line('Connecting…')]);
    } else if (_go && s.running && _error == null) {
      working.addAll([const _Spinner(), line('Getting your model…')]);
    } else {
      // Something to agree to: say what it will do, then one button.
      final starter = s.starter;
      final plan = [
        if (!s.installed && s.install.available) s.install.summary,
        if (starter != null)
          'Then it downloads ${starter.model} (${starter.sizeGb} GB). '
              '${starter.blurb}',
      ].join(' ');
      if (plan.isNotEmpty) working.add(line(plan));
      for (final w in s.warnings) {
        working.add(line(w, color: c.warn));
      }
      action = Row(
        children: [
          FilledButton(
            key: const ValueKey('local-setup'),
            style: style,
            onPressed: s.tooOld ? null : _setUp,
            child: const Text('Set up with Ollama'),
          ),
          if (!s.installed) ...[
            const SizedBox(width: 8),
            TextButton(
              key: const ValueKey('local-get'),
              onPressed: () => openUrl(s.installPage),
              child: const Text('Get Ollama ↗'),
            ),
          ],
        ],
      );
    }
    if (_error case final e? when !_startFailed) {
      working.add(line(e, color: c.error));
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const Text(
          'Set up with Ollama',
          style: TextStyle(fontSize: 14.5, fontWeight: FontWeight.w600),
        ),
        const SizedBox(height: 3),
        Text(
          'Private and offline, good for everyday tasks.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
        const SizedBox(height: 12),
        ...working,
        if (action != null) ...[const SizedBox(height: 10), action],
        const SizedBox(height: 14),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            key: const ValueKey('use-api'),
            onPressed: widget.onUseApi,
            child: Text(
              'Use an API model instead',
              style: TextStyle(color: c.muted),
            ),
          ),
        ),
        Text(
          'Cloud models are stronger on long multi-step work.',
          style: TextStyle(fontSize: 11.5, color: c.muted),
        ),
      ],
    );
  }

  static String _phase(LocalInstalling i) => switch (i.phase) {
    'downloading' => 'Downloading Ollama…',
    'verifying' => 'Checking the download…',
    'installing' => 'Installing…',
    _ => 'Getting ready…',
  };
}

class _Spinner extends StatelessWidget {
  const _Spinner();

  @override
  Widget build(BuildContext context) => const Align(
    alignment: Alignment.centerLeft,
    child: SizedBox.square(
      dimension: 16,
      child: CircularProgressIndicator(strokeWidth: 2),
    ),
  );
}
