import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'widgets.dart';

/// The oldest open sign-in or connect card, over the whole window. Nothing
/// when none is open. More than one waits behind it, in arrival order.
class AuthCardOverlay extends StatelessWidget {
  const AuthCardOverlay({super.key, required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: store,
    builder: (context, _) {
      final p = store.authPrompts.firstOrNull;
      if (p == null) return const SizedBox.shrink();
      return Stack(
        children: [
          const ModalBarrier(color: Colors.black38, dismissible: false),
          Center(
            child: AuthCardView(
              // A new card for the same call is a fresh state.
              key: ValueKey('${p.id}/${p.card.url}/${p.card.userCode}'),
              store: store,
              prompt: p,
              waiting: store.authPrompts.length - 1,
            ),
          ),
        ],
      );
    },
  );
}

/// Only web pages go to the OS opener.
bool _web(String? url) =>
    url != null && (url.startsWith('https://') || url.startsWith('http://'));

class AuthCardView extends StatefulWidget {
  const AuthCardView({
    super.key,
    required this.store,
    required this.prompt,
    this.waiting = 0,
  });

  final TernStore store;
  final AuthPrompt prompt;

  /// Other cards queued behind this one.
  final int waiting;

  @override
  State<AuthCardView> createState() => _AuthCardViewState();
}

class _AuthCardViewState extends State<AuthCardView> {
  var _busy = false;
  String? _error;

  AuthCard get _card => widget.prompt.card;

  Future<void> _answer(String answer) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await widget.store.answerAuthPrompt(widget.prompt, answer);
    if (mounted) {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    final card = _card;
    final who = widget.prompt.agentLabel;
    return ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 460),
      child: Card(
        elevation: 6,
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                card.title,
                style: const TextStyle(
                  fontSize: 16,
                  fontWeight: FontWeight.w600,
                ),
              ),
              if (who != null)
                Padding(
                  padding: const EdgeInsets.only(top: 2),
                  child: Text('Asked by $who', style: small),
                ),
              const SizedBox(height: 12),
              if (card.choose) ..._choose(c) else ..._signIn(c, small),
              if (card.notes.isNotEmpty) ...[
                const SizedBox(height: 10),
                for (final n in card.notes) Text(n, style: small),
              ],
              if (_error case final e?) ...[
                const SizedBox(height: 8),
                Text(e, style: TextStyle(fontSize: 12, color: c.error)),
              ],
              const SizedBox(height: 8),
              Row(
                children: [
                  if (widget.waiting > 0)
                    Text('${widget.waiting} more waiting', style: small),
                  const Spacer(),
                  TextButton(
                    onPressed: _busy ? null : () => _answer('cancel'),
                    child: const Text('Cancel'),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  List<Widget> _choose(TernColors c) => [
    for (final o in _card.options)
      Padding(
        padding: const EdgeInsets.only(bottom: 10),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (o.recommended)
              FilledButton(
                onPressed: _busy ? null : () => _answer(o.id),
                child: Text('${o.label} (recommended)'),
              )
            else
              OutlinedButton(
                onPressed: _busy ? null : () => _answer(o.id),
                child: Text(o.label),
              ),
            if (o.detail.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(
                  o.detail,
                  style: TextStyle(fontSize: 12, color: c.muted),
                ),
              ),
          ],
        ),
      ),
  ];

  List<Widget> _signIn(TernColors c, TextStyle? small) {
    final card = _card;
    final page = card.device ? card.verificationUri : card.url;
    return [
      if (card.device) ...[
        Text('Enter this code', style: small),
        const SizedBox(height: 4),
        Row(
          children: [
            Expanded(
              child: SelectableText(
                card.userCode!,
                style: const TextStyle(
                  fontSize: 28,
                  fontWeight: FontWeight.w600,
                  letterSpacing: 2,
                  fontFamily: 'monospace',
                ),
              ),
            ),
            CopyButton(text: () => card.userCode!, tooltip: 'Copy code'),
          ],
        ),
        const SizedBox(height: 8),
      ],
      if (_web(page))
        FilledButton(
          onPressed: () => openUrl(page!),
          child: Text(card.device ? 'Open $page' : 'Open sign-in page'),
        ),
      if (page != null && !card.device) ...[
        const SizedBox(height: 6),
        SelectableText(page, style: TextStyle(fontSize: 11, color: c.muted)),
      ],
      if (card.deviceAvailable)
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            onPressed: _busy ? null : () => _answer('device'),
            child: const Text('Use a code instead'),
          ),
        ),
      const SizedBox(height: 8),
      Row(
        children: [
          const SizedBox(
            width: 12,
            height: 12,
            child: CircularProgressIndicator(strokeWidth: 2),
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Text('Waiting for you to finish signing in…', style: small),
          ),
        ],
      ),
    ];
  }
}
