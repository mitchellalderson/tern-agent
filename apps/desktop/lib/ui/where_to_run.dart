import 'dart:io';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/connection.dart';
import '../state/daemon.dart';
import '../theme.dart';

/// On first launch, ask once where tern runs: on this computer (with
/// Start at login, so routines keep working with the window closed), or
/// on a tern server this app pairs with. tern Cloud gets a row that's
/// disabled until it exists ([CloudOffer]).
///
/// Someone whose tern already starts at login isn't asked: it runs here.
/// Dismissing the question also means "here", without Start at login.
Future<void> maybeAskWhereToRun(
  BuildContext context,
  TernControl control,
  Connections connections,
) async {
  if (connections.current != null) return;
  if (control.bin != null) {
    final state = await control.loginState();
    if (state?.atLogin ?? control.startsAtLogin) {
      await connections.useLocal();
      return;
    }
  }
  if (!context.mounted) return;
  final chosen = await showDialog<bool>(
    context: context,
    builder: (_) =>
        WhereToRunDialog(control: control, connections: connections),
  );
  if (chosen != true) await connections.useLocal();
}

enum _Where { here, server, account, cloud }

class WhereToRunDialog extends StatefulWidget {
  const WhereToRunDialog({
    super.key,
    required this.control,
    required this.connections,
  });

  final TernControl control;
  final Connections connections;

  @override
  State<WhereToRunDialog> createState() => _WhereToRunDialogState();
}

class _WhereToRunDialogState extends State<WhereToRunDialog> {
  var _where = _Where.here;
  var _atLogin = true;
  final _server = ServerFields();
  final _accountUrl = TextEditingController();
  var _busy = false;
  String? _error;

  @override
  void dispose() {
    _server.dispose();
    _accountUrl.dispose();
    super.dispose();
  }

  Future<void> _continue() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      switch (_where) {
        case _Where.here:
          await widget.connections.useLocal();
          if (_atLogin && widget.control.bin != null) {
            await widget.control.setStartAtLogin(true);
          }
        case _Where.server:
          await widget.connections.connect(_server.link());
        case _Where.account:
          final account = widget.connections.account;
          if (account == null) return;
          final url = parseServerUrl(_accountUrl.text);
          if (!account.signedIn) await account.signIn();
          await widget.connections.connectAccount(url);
        case _Where.cloud:
          // Not offered yet (CloudOffer.available).
          return;
      }
      if (mounted) Navigator.pop(context, true);
    } on Exception catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = errorText(e);
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    final canRunHere = widget.control.bin != null;
    return AlertDialog(
      title: const Text('Where should tern run?'),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460, minWidth: 420),
        child: SingleChildScrollView(
          child: RadioGroup<_Where>(
            groupValue: _where,
            onChanged: (v) {
              if (v != null && !_busy) setState(() => _where = v);
            },
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                RadioListTile<_Where>(
                  contentPadding: EdgeInsets.zero,
                  value: _Where.here,
                  title: Text(
                    Platform.isMacOS ? 'On this Mac' : 'On this computer',
                  ),
                  subtitle: Text(
                    Platform.isMacOS
                        ? 'tern runs here in the background, with an icon in '
                              'the menu bar.'
                        : 'tern runs here in the background.',
                    style: small,
                  ),
                ),
                if (_where == _Where.here && canRunHere)
                  Padding(
                    padding: const EdgeInsets.only(left: 40),
                    child: CheckboxListTile(
                      contentPadding: EdgeInsets.zero,
                      dense: true,
                      controlAffinity: ListTileControlAffinity.leading,
                      value: _atLogin,
                      onChanged: _busy
                          ? null
                          : (v) => setState(() => _atLogin = v ?? false),
                      title: const Text('Start tern when I log in'),
                      subtitle: Text(
                        'So routines and Telegram keep working when this '
                        'window is closed.',
                        style: small,
                      ),
                    ),
                  ),
                RadioListTile<_Where>(
                  contentPadding: EdgeInsets.zero,
                  value: _Where.server,
                  title: const Text('On a tern server'),
                  subtitle: Text(
                    'Connect to tern running on another computer you own.',
                    style: small,
                  ),
                ),
                if (_where == _Where.server)
                  Padding(
                    padding: const EdgeInsets.only(left: 40, bottom: 4),
                    child: ServerForm(fields: _server, enabled: !_busy),
                  ),
                if (widget.connections.account case final account?) ...[
                  const RadioListTile<_Where>(
                    contentPadding: EdgeInsets.zero,
                    value: _Where.account,
                    title: Text('Sign in'),
                    subtitle: Text('Use your tern account.'),
                  ),
                  if (_where == _Where.account)
                    Padding(
                      padding: const EdgeInsets.only(left: 40, bottom: 4),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.stretch,
                        children: [
                          if (account.email case final email?)
                            Padding(
                              padding: const EdgeInsets.only(bottom: 8),
                              child: Text('Signed in as $email', style: small),
                            ),
                          TextField(
                            controller: _accountUrl,
                            enabled: !_busy,
                            autofocus: true,
                            decoration: const InputDecoration(
                              labelText: 'Server address',
                              hintText: 'https://tern.example.com',
                            ),
                          ),
                        ],
                      ),
                    ),
                ] else
                  const RadioListTile<_Where>(
                    contentPadding: EdgeInsets.zero,
                    value: _Where.cloud,
                    enabled: CloudOffer.available,
                    title: Text(CloudOffer.title),
                    subtitle: Text(CloudOffer.subtitle),
                  ),
                if (_error case final e?) ...[
                  const SizedBox(height: 8),
                  Text(e, style: TextStyle(fontSize: 12, color: c.error)),
                ],
              ],
            ),
          ),
        ),
      ),
      actions: [
        FilledButton(
          onPressed: _busy ? null : _continue,
          child: Text(
            _busy
                ? switch (_where) {
                    _Where.server => 'Connecting…',
                    _Where.account => 'Signing in…',
                    _ => 'Setting up…',
                  }
                : 'Continue',
          ),
        ),
      ],
    );
  }
}

/// A user-facing sentence for a failure while choosing.
String errorText(Object e) => serverErrorText(e);

/// The text fields of [ServerForm].
class ServerFields {
  final input = TextEditingController();
  final code = TextEditingController();

  /// Throws [FormatException] with a message to show.
  PairingLink link() => parseServerInput(input.text, code: code.text);

  /// Just where the server is (and its certificate pin, from a pairing
  /// link), for testing it before there is a code. Throws
  /// [FormatException] with a message to show.
  ({Uri url, String? pin}) address() {
    final text = input.text.trim();
    if (text.startsWith('tern://')) {
      final l = parseServerInput(text);
      return (url: l.url, pin: l.fingerprint);
    }
    if (text.isEmpty) {
      throw const FormatException('Paste a pairing link or a server URL.');
    }
    return (url: parseServerUrl(text), pin: null);
  }

  void dispose() {
    input.dispose();
    code.dispose();
  }
}

/// "Pairing link or server URL", plus the code when it's a bare URL.
class ServerForm extends StatefulWidget {
  const ServerForm({
    super.key,
    required this.fields,
    this.enabled = true,
    this.autofocus = true,
  });

  final ServerFields fields;
  final bool enabled;
  final bool autofocus;

  @override
  State<ServerForm> createState() => _ServerFormState();
}

class _ServerFormState extends State<ServerForm> {
  @override
  void initState() {
    super.initState();
    widget.fields.input.addListener(_changed);
  }

  @override
  void dispose() {
    widget.fields.input.removeListener(_changed);
    super.dispose();
  }

  void _changed() => setState(() {});

  @override
  Widget build(BuildContext context) {
    final small = Theme.of(context).textTheme.bodySmall;
    final f = widget.fields;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        TextField(
          controller: f.input,
          enabled: widget.enabled,
          autofocus: widget.autofocus,
          decoration: const InputDecoration(
            labelText: 'Pairing link or server URL',
            hintText: 'tern://pair?… or https://home.example.net:7420',
          ),
        ),
        if (f.input.text.trim().isNotEmpty && needsCode(f.input.text))
          TextField(
            controller: f.code,
            enabled: widget.enabled,
            decoration: const InputDecoration(labelText: 'Pairing code'),
          ),
        const SizedBox(height: 6),
        Text(
          'On the server, run `tern devices pair` (or use Add device in its '
          'app) and paste the link it shows.',
          style: small,
        ),
      ],
    );
  }
}
