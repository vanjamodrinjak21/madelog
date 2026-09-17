package slug

// Adjectives and Nouns are small curated lists of neutral English words.
// Keep them lowercase ASCII, three letters or more, and free of anything that
// could combine into something unfortunate: these end up in public URLs.

// Adjectives holds the first component of a slug.
var Adjectives = []string{
	"amber", "ancient", "autumn", "brave", "brief", "bright", "brisk", "calm",
	"candid", "clear", "clever", "cobalt", "cool", "copper", "crisp", "curious",
	"daily", "damp", "dawn", "deep", "dry", "dusty", "eager", "early",
	"easy", "even", "fair", "fierce", "flat", "fleet", "fond", "fresh",
	"gentle", "glad", "golden", "grand", "green", "happy", "hidden", "humble",
	"icy", "idle", "ivory", "jolly", "keen", "kind", "late", "level",
	"light", "linen", "little", "lively", "lone", "loud", "lucky", "mellow",
	"merry", "mild", "misty", "modest", "narrow", "neat", "nimble", "noble",
	"north", "olive", "open", "patient", "plain", "polite", "proud", "quick",
	"quiet", "rapid", "ready", "rough", "round", "royal", "rustic", "sandy",
	"shady", "sharp", "silent", "silver", "simple", "slate", "small", "smooth",
	"snowy", "solid", "south", "spry", "steady", "stern", "still", "stout",
	"sunny", "swift", "tall", "tidy", "tiny", "true", "twin", "upper",
	"urban", "vast", "velvet", "warm", "west", "wide", "willing", "windy",
	"winter", "wise", "witty", "woven", "young", "zesty",
}

// Nouns holds the second component of a slug.
var Nouns = []string{
	"acorn", "alder", "anchor", "arbor", "arrow", "aspen", "atlas", "badger",
	"basin", "beacon", "birch", "bison", "bloom", "boulder", "branch", "bridge",
	"brook", "burrow", "canyon", "cedar", "cinder", "cliff", "clover", "comet",
	"compass", "coral", "cotton", "crane", "creek", "crest", "dahlia", "delta",
	"dune", "eagle", "ember", "fable", "falcon", "fennel", "fern", "field",
	"finch", "forest", "fossil", "garden", "glacier", "glade", "granite", "grove",
	"harbor", "harvest", "hazel", "heron", "hollow", "ibis", "island", "ivy",
	"juniper", "kestrel", "lantern", "laurel", "ledge", "lichen", "lily", "linnet",
	"lumen", "magnet", "maple", "marble", "meadow", "mesa", "moss", "otter",
	"pebble", "pine", "plateau", "plover", "pollen", "poplar", "prairie", "quarry",
	"quartz", "quill", "rapids", "raven", "reef", "ridge", "river", "rowan",
	"sable", "sage", "sail", "sequoia", "shale", "shore", "sierra", "sparrow",
	"spruce", "stone", "summit", "sylvan", "thicket", "thistle", "timber", "trail",
	"tundra", "valley", "vane", "vessel", "vine", "walnut", "willow", "wombat",
	"yarrow", "zephyr",
}
