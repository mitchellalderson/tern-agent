import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../theme.dart';

/// Code auto mode (docs/alpha-coding-agent.md): per-agent, opt-in, not fully
/// sandboxed. These are its disclaimer, the confirm dialog that must be
/// answered before it turns on, and the header chip that shows while it
/// applies. The copy matches `code_auto_disclaimer` in tern-core.
const codeAutoDisclaimerVersion = 1;

/// What the mode's header chip says right now.
enum CodeAutoChipState {
  /// Edits files and runs build and test commands without asking.
  on,

  /// The OS sandbox isn't confirmed active: only file edits are automatic,
  /// commands ask as usual.
  editsOnly,
}

/// The sentence list under "What it does not protect", shared by the dialog
/// and the selector's explainer.
const codeAutoLimits = <String>[
  'Anything in this folder can be changed or deleted, including uncommitted '
      'work. tern saves a snapshot before each run\'s first change and the run '
      'can be undone. It does not protect your .git history.',
  'The code it writes runs on your computer. Tests, build scripts and '
      'install scripts can do anything the sandbox allows, and tern does not '
      'read them first.',
  'The sandbox is an operating-system feature, not a virtual machine. It does '
      'not cover reading files outside your home folder, local sockets '
      '(including Docker), or anything you run yourself later: cargo run, '
      'make, an editor task or a git hook. Review what it changed before you do.',
  'If tern cannot confirm the sandbox is active, commands ask as usual and '
      'this mode only covers file edits.',
  'Reading a web page or other outside content pauses auto mode for the rest '
      'of that run.',
];

/// The confirm dialog shown before code auto mode turns on. Resolves to true
/// only when the user ticks "I understand" and presses Turn on.
class CodeAutoConfirmDialog extends StatefulWidget {
  const CodeAutoConfirmDialog({
    super.key,
    required this.agentName,
    this.folder,
    this.raisesFrom,
  });

  final String agentName;

  /// The folder it will work in, when known (else "its workspace folder").
  final String? folder;

  /// The ceiling this raises (`supervised`), when the agent's is below
  /// `code`; null when there is nothing to raise.
  final String? raisesFrom;

  static Future<bool> show(
    BuildContext context, {
    required String agentName,
    String? folder,
    String? raisesFrom,
  }) async =>
      await showDialog<bool>(
        context: context,
        builder: (_) => CodeAutoConfirmDialog(
          agentName: agentName,
          folder: folder,
          raisesFrom: raisesFrom,
        ),
      ) ??
      false;

  @override
  State<CodeAutoConfirmDialog> createState() => _CodeAutoConfirmState();
}

class _CodeAutoConfirmState extends State<CodeAutoConfirmDialog> {
  bool _understood = false;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    return AlertDialog(
      key: const Key('code-auto-dialog'),
      title: const Text('Code auto mode: not fully sandboxed'),
      content: SizedBox(
        width: 460,
        child: SingleChildScrollView(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                '${widget.agentName} will edit files and run build and test '
                'commands in ${widget.folder ?? 'its workspace folder'} '
                'without asking you each time.',
              ),
              const SizedBox(height: 10),
              Text(
                'What limits it. Commands run inside the operating-system '
                'sandbox, checked just now: they can write only in this '
                'folder and have no network. Pushing code, installing '
                'packages, network access, deleting much of the folder and '
                'anything outside it still ask.',
                style: small,
              ),
              const SizedBox(height: 10),
              const Text(
                'What it does not protect.',
                style: TextStyle(fontWeight: FontWeight.w600),
              ),
              for (final l in codeAutoLimits)
                Padding(
                  padding: const EdgeInsets.only(top: 4),
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text('•  ', style: TextStyle(color: c.muted)),
                      Expanded(child: Text(l, style: small)),
                    ],
                  ),
                ),
              const SizedBox(height: 10),
              const Text(
                'Use it only on code you would be willing to run yourself.',
                style: TextStyle(fontWeight: FontWeight.w600),
              ),
              if (widget.raisesFrom case final from?)
                Padding(
                  padding: const EdgeInsets.only(top: 10),
                  child: Text(
                    'This raises ${widget.agentName}\'s ceiling from $from to '
                    'code.',
                    key: const Key('code-auto-raises'),
                    style: TextStyle(color: c.warn),
                  ),
                ),
              const SizedBox(height: 6),
              CheckboxListTile(
                key: const Key('code-auto-understand'),
                value: _understood,
                dense: true,
                contentPadding: EdgeInsets.zero,
                controlAffinity: ListTileControlAffinity.leading,
                title: const Text('I understand.'),
                onChanged: (v) => setState(() => _understood = v ?? false),
              ),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          key: const Key('code-auto-cancel'),
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const Key('code-auto-confirm'),
          onPressed: _understood ? () => Navigator.of(context).pop(true) : null,
          child: const Text('Turn on'),
        ),
      ],
    );
  }
}

/// The header chip while code auto mode applies to the agent in front:
/// amber `Code auto`, or grey `Code auto · edits only` when the OS sandbox
/// isn't confirmed active (commands ask as usual).
class CodeAutoChip extends StatelessWidget {
  const CodeAutoChip({super.key, required this.state});

  final CodeAutoChipState state;

  /// Whether `agent` is opted in and its ceiling lets the mode apply.
  static bool applies(Agent agent) =>
      agent.codeAuto &&
      switch (agent.autonomyLevel) {
        null => true,
        final l => l.index >= AutonomyLevel.code.index,
      };

  /// The state from the sandbox's verification (`/v1/info`); unknown counts
  /// as not confirmed.
  static CodeAutoChipState stateFor(bool? sandboxVerified) =>
      sandboxVerified == true
      ? CodeAutoChipState.on
      : CodeAutoChipState.editsOnly;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final on = state == CodeAutoChipState.on;
    final color = on ? c.warn : c.muted;
    return Tooltip(
      message: on
          ? 'Edits files and runs commands in its folder without asking. '
                'Not fully sandboxed.'
          : 'The sandbox isn\'t confirmed active, so commands still ask; '
                'only file edits are automatic.',
      child: Container(
        key: Key(on ? 'code-auto-chip' : 'code-auto-chip-edits-only'),
        padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
        decoration: BoxDecoration(
          border: Border.all(color: color),
          borderRadius: BorderRadius.circular(10),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.terminal, size: 12, color: color),
            const SizedBox(width: 4),
            Text(
              on ? 'Code auto' : 'Code auto · edits only',
              style: TextStyle(fontSize: 11.5, color: color),
            ),
          ],
        ),
      ),
    );
  }
}
