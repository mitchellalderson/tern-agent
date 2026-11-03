import 'models.dart';

/// A tool as the model sees it: what it's for and the arguments it takes.
class ToolSpec {
  const ToolSpec({
    required this.name,
    required this.description,
    required this.inputSchema,
  });

  factory ToolSpec.fromJson(Json j) => ToolSpec(
    name: j['name'] as String,
    description: j['description'] as String? ?? '',
    inputSchema: j['input_schema'] as Json? ?? const {},
  );

  final String name;
  final String description;

  /// JSON Schema for the arguments.
  final Json inputSchema;

  /// Top-level arguments, required ones first.
  List<({String name, String type, String description, bool required})>
  get params {
    final props = inputSchema['properties'] as Json? ?? const {};
    final required = {...?(inputSchema['required'] as List?)};
    final out = [
      for (final MapEntry(:key, :value) in props.entries)
        (
          name: key,
          type: _type(value),
          description: (value as Json?)?['description'] as String? ?? '',
          required: required.contains(key),
        ),
    ];
    out.sort((a, b) => a.required == b.required ? 0 : (a.required ? -1 : 1));
    return out;
  }

  static String _type(Object? schema) {
    if (schema is! Json) return 'any';
    final t = schema['type'];
    if (t is List) return t.join(' | ');
    if (t == 'array') return '${_type(schema['items'])}[]';
    if (schema['enum'] case final List e)
      return e.map((v) => '"$v"').join(' | ');
    return t as String? ?? 'any';
  }
}
