import groovy.json.JsonBuilder
import org.json.JSONObject
import org.everit.json.schema.loader.SchemaLoader
import org.everit.json.schema.ValidationException

// The leaf violations of everit, as the NfcoreSchema class of older nf-core pipelines logs
// them; workflows/nextflow/runtime/tests/legacy.rs checks the same schema and params.
def text = '{"$schema": "http://json-schema.org/draft-07/schema", "definitions": {"g": {"properties": {"n": {"type": "integer"}, "skip": {"type": "integer"}, "mode": {"type": "string", "enum": ["a", "b"]}}, "required": ["input"]}}, "allOf": [{"$ref": "#/definitions/g"}]}'
def collect
collect = { json, found ->
    def causes = json['causingExceptions']
    if (causes.length() == 0) {
        found << "${json['pointerToViolation']}: ${json['message']}"
    }
    for (cause in causes) {
        collect(cause, found)
    }
}
def found = []
try {
    SchemaLoader.load(new JSONObject(text)).validate(new JSONObject(new JsonBuilder([n: 'x', mode: 'c']).toString()))
} catch (ValidationException e) {
    collect(e.toJSON(), found)
}
println(found.sort().join('\n'))
