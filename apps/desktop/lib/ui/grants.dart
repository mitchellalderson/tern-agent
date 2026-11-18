import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';

/// Settings › Permissions › Always allowed: what "Always allow this" has
/// granted, each revocable. The list is what `tern grants` shows.
class GrantsSection extends StatefulWidget {
  const GrantsSection({super.key, required this.store});

  final TernStore store;

  @override
  State<GrantsSection> createState() => _GrantsSectionState();
}

class _GrantsSectionState extends State<GrantsSection> {
  List<Grant>? _grants;
  String? _error;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final g = await widget.store.client.grants();
      if (mounted) setState(() => _grants = g);
    } on TernException catch (e) {
      // An older daemon has no grants list.
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _revoke(Grant g) async {
    try {
      await widget.store.client.revokeGrant(g);
    } on TernException catch (e) {
      if (e.status != 404) {
        if (mounted) setState(() => _error = e.message);
        return;
      }
    }
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final grants = _grants;
    if (grants == null) {
      return Text(
        _error ?? 'Loading…',
        style: TextStyle(fontSize: 12.5, color: c.muted),
      );
    }
    if (grants.isEmpty) {
      return Text(
        'Nothing is always allowed. tern offers it after you allow the same '
        'thing once, three times.',
        style: TextStyle(fontSize: 12.5, color: c.muted),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        for (final g in grants)
          ListTile(
            key: ValueKey('grant-${g.tool}-${g.pattern}'),
            contentPadding: EdgeInsets.zero,
            dense: true,
            title: Text(g.tool, style: const TextStyle(fontSize: 13)),
            subtitle: Text(
              g.note,
              style: TextStyle(fontSize: 11.5, color: c.muted),
            ),
            trailing: TextButton(
              onPressed: () => _revoke(g),
              child: const Text('Revoke'),
            ),
          ),
        if (_error case final e?)
          Text(e, style: TextStyle(fontSize: 12, color: c.error)),
      ],
    );
  }
}
