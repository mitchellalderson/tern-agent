// "Run on this Mac": the first-run choice that uses Ollama instead of a
// cloud provider (docs/local-models.md). It shows one of four states —
// not installed, installing, installed but not running, running — and
// hands over when the server is ready. Nothing is downloaded until the
// person presses "Download and install", under a sentence saying what that
// does.
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../theme.dart';
import 'widgets.dart';

class LocalSetup extends StatefulWidget {
  const LocalSetup({super.key, required this.client, required this.onReady});

  final TernClient client;

  /// Ollama runs and is new enough: carry on to the models.
  final VoidCallback onReady;

  @override
  State<LocalSetup> createState() => _LocalSetupState();
}

class _LocalSetupState extends State<LocalSetup> {
  LocalStatus? _status;
  String? _error;
  var _busy = false;
  Timer? _poll;

  @override
  void initState() {
    super.initState();
    _load();
    // The daemon also sends `local_changed`; polling keeps this step
    // self-contained and is a local call.
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
      if (mounted) setState(() => _status = s);
    } on Object {
      // Keep what is shown; the next poll tries again.
    }
  }

  Future<void> _do(Future<LocalStatus> Function() action) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final s = await action();
      if (mounted) setState(() => _status = s);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  static String _phase(LocalInstalling i) => switch (i.phase) {
    'downloading' => 'Downloading Ollama…',
    'verifying' => 'Checking the download…',
    'installing' => 'Installing…',
    _ => 'Getting ready…',
  };

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = _status;
    final style = FilledButton.styleFrom(
      backgroundColor: c.accent,
      foregroundColor: c.onAccent,
    );
    Widget note(String text, {Color? color}) => Padding(
      padding: const EdgeInsets.only(top: 6),
      child: Text(text, style: TextStyle(fontSize: 12.5, color: color ?? c.muted)),
    );
    final children = <Widget>[
      const Text(
        'Run on this Mac',
        style: TextStyle(fontSize: 14.5, fontWeight: FontWeight.w600),
      ),
      const SizedBox(height: 3),
      Text(
        'Private and offline, good for everyday tasks. Runs with Ollama.',
        style: Theme.of(context).textTheme.bodySmall,
      ),
      const SizedBox(height: 14),
    ];
    if (s == null) {
      children.add(
        const Center(
          child: SizedBox.square(
            dimension: 18,
            child: CircularProgressIndicator(strokeWidth: 2),
          ),
        ),
      );
    } else if (s.installing.working) {
      children.addAll([
        LinearProgressIndicator(value: s.installing.fraction, minHeight: 3),
        note(_phase(s.installing)),
      ]);
    } else if (!s.installed) {
      children.addAll([
        const Text('Ollama isn\'t on this Mac yet.'),
        if (s.install.available) note(s.install.summary),
        const SizedBox(height: 10),
        Wrap(
          spacing: 8,
          runSpacing: 4,
          crossAxisAlignment: WrapCrossAlignment.center,
          children: [
            if (s.install.available)
              FilledButton(
                key: const ValueKey('local-install'),
                style: style,
                onPressed: _busy
                    ? null
                    : () => _do(() => widget.client.localInstall(consent: true)),
                child: const Text('Download and install'),
              ),
            TextButton(
              key: const ValueKey('local-get'),
              onPressed: () => openUrl(s.installPage),
              child: const Text('Get it from ollama.com ↗'),
            ),
          ],
        ),
        if (s.installing.failed) note(s.installing.message ?? 'The install failed.', color: c.error),
      ]);
    } else if (!s.running) {
      children.addAll([
        const Text('Ollama is installed but not running.'),
        const SizedBox(height: 10),
        Align(
          alignment: Alignment.centerLeft,
          child: FilledButton(
            key: const ValueKey('local-start'),
            style: style,
            onPressed: _busy ? null : () => _do(widget.client.localStart),
            child: const Text('Start Ollama'),
          ),
        ),
      ]);
    } else {
      children.addAll([
        Row(
          children: [
            Icon(Icons.check_circle, size: 16, color: c.ok),
            const SizedBox(width: 6),
            Text(
              s.version == null
                  ? 'Ollama is running'
                  : 'Ollama ${s.version} is running',
              style: TextStyle(fontSize: 12.5, color: c.ok),
            ),
          ],
        ),
        for (final w in s.warnings) note(w, color: c.warn),
        const SizedBox(height: 10),
        Align(
          alignment: Alignment.centerLeft,
          child: FilledButton(
            key: const ValueKey('local-continue'),
            style: style,
            // Too old to use: the warning says what to do.
            onPressed: s.tooOld ? null : widget.onReady,
            child: const Text('Continue'),
          ),
        ),
      ]);
    }
    if (_error case final e?) {
      children.add(note(e, color: c.error));
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: children,
    );
  }
}
