import 'package:flutter/material.dart';

import '../theme.dart';

/// One strip over the app when the running tern is not the version this app
/// came with. A restart picks up the bundled one.
class VersionBanner extends StatefulWidget {
  const VersionBanner({super.key, required this.message, this.onRestart});

  final String message;

  /// Restarts tern; without it the banner only explains.
  final Future<void> Function()? onRestart;

  @override
  State<VersionBanner> createState() => _VersionBannerState();
}

class _VersionBannerState extends State<VersionBanner> {
  var _busy = false;
  String? _error;

  Future<void> _restart() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await widget.onRestart!();
    } on Object catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      key: const Key('version-banner'),
      color: c.card,
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      child: Row(
        children: [
          Icon(Icons.info_outline, size: 16, color: c.warn),
          const SizedBox(width: 10),
          Expanded(
            child: Text(
              _error == null
                  ? widget.message
                  : '${widget.message}\nRestart failed: $_error',
              style: TextStyle(fontSize: 12.5, color: c.muted),
            ),
          ),
          if (widget.onRestart != null)
            TextButton(
              onPressed: _busy ? null : _restart,
              child: const Text('Restart tern'),
            ),
        ],
      ),
    );
  }
}
