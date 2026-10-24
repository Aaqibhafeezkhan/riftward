# Races, backgrounds and character creation

[Design summary](README.md) · Status: designed in outline; races chosen, backgrounds not yet

A new character's starting skills come from two choices: their **race** and their **background**. There are still no classes. Race and background only decide where a character starts; from there, they learn and use whatever skills they like.

## Character creation

1. **Name** the character (built).
2. **Choose a race.** It grants racial skills, usually passive.
3. **Spend starting Karma** with `raise <attribute>` (built: 10 Karma, attributes start at 1, Magic at 0). Karma spent on Magic is latent talent: it does nothing until the character knows a casting skill.
4. **Choose a background.** It grants a few starting skills. Backgrounds can require attributes or a race; Awakened backgrounds, which grant a casting skill, require Magic 1 or more. Karma comes before background so those gates can be met.

Karma spending is built today as an in-game command; character creation will need its own spending step before the background choice. Unspent Karma carries into play as now.

Every character also gets the universal starting skills, such as a basic attack and perhaps Foraging, so everyone can fight and survive from the first minute.

Skills granted at creation are recorded with `learned_via: Starting` and start at level 0, then improve through use like any other skill.

## Races

The starting races are the eight from Earthdawn. Each race's traditional abilities become racial skills; the skill names and details below are proposals to tune.

| Race | In brief | Racial skills (proposed) | Attribute leanings |
| --- | --- | --- | --- |
| Dwarf | Stout, stubborn, crafters and miners | **Heat Sight** (passive): see warm bodies in darkness | Body, Strength |
| Elf | Graceful, long-lived, close to nature | **Low-Light Vision** (passive): see in dim light | Agility, Intuition |
| Human | Adaptable, found everywhere | **Versatility** (passive): practice builds faster, so new skills warm up sooner | None |
| Obsidiman | Living stone, huge, slow and patient | **Stone Skin** (passive, triggers on taking damage): natural armour | Body, Strength; low Reaction |
| Ork | Fierce, quick to anger, hardy | **Low-Light Vision** (passive); **Gahad** (active): a burst of rage | Strength, Body |
| Troll | Towering, proud, warrior culture | **Heat Sight** (passive) | Strength, Body |
| T'skrang | Lizard-folk of the rivers, flamboyant | **Tail Combat** (active): an extra attack skill; **Swimming** (passive) | Agility, Charisma |
| Windling | Tiny, winged, curious | **Flight** (passive); **Astral Sight** (active): see magic and spirits | Agility; low Strength |

- **Size matters later.** Windlings can't wield large weapons, and trolls and obsidimen need bigger gear. That belongs to the item system.
- **Attribute leanings only apply if races adjust attributes,** which is still open. If they do, a small starting adjustment (±1) would be enough: attributes are uncapped and contests compare ratios, so it shapes early play without deciding late play.

Racial abilities are ordinary skills that require the race, using a `Race` condition (see [Hidden skills](skills.md#hidden-skills) for the condition language):

```ron
"darkvision": (
    name: "Darkvision",
    kind: Passive(triggers: [EnterDarkRoom]),
    requires: [Race("elf")],
    grant: Race("elf"),           // granted at creation to characters who meet it
),
```

- **Racial skills can be taught,** like every skill, but only to students who meet their requirements, which means the same race. An elf master can teach Darkvision to another elf.
- **Partial overlaps are just conditions:** `requires: [Any([Race("elf"), Race("half_elf")])]`.

## Backgrounds

A background is a life before play: a few starting skills and, for some, a magical tradition. Any background can be gated by a `requires` condition, in the same [condition language](skills.md#hidden-skills) as skills, so it can demand attributes, a race, or combinations. The names, grants and gates below are examples to tune.

| Background | Requires (examples) | Starting skills (examples) | Awakened |
| --- | --- | --- | --- |
| Soldier | Body 2 | Swordplay basics, Toughness | No |
| Poacher | Agility 2 | Archery basics, Stealth basics | No |
| Scholar | Logic 2 | Lore, Logic-based crafting | No |
| Acolyte | Magic 1, Willpower 2 | Divine foundation skill | Yes: Divine |
| Mystic | Magic 1, Intuition 2 | Shamanic foundation skill | Yes: Shamanic |
| Apprentice | Magic 1, Logic 2 | Hermetic foundation skill | Yes: Hermetic |
| Adept | Magic 1, Body 2 | Adept foundation skill | Yes: Adept |

Backgrounds are data, like skill trees:

```ron
// assets/backgrounds.ron
{
    "soldier": (
        name: "Soldier",
        requires: Stat("body", 2),
        skills: ["swordplay:basics", "endurance:toughness"],
    ),
    "acolyte": (
        name: "Acolyte",
        requires: All([Stat("magic", 1), Stat("willpower", 2)]),
        skills: ["divine:foundation"],
    ),
    "river_pilot": (
        name: "River Pilot",
        requires: Race("tskrang"),
        skills: ["sailing:basics"],
    ),
}
```

- **Gates follow what a background implies.** A soldier is sturdy and a scholar thinks hard, so their gates name those attributes. Backgrounds with no gate keep an option open for everyone.
- **Gates must be affordable from starting Karma.** With 10 Karma and every attribute at 1, a gate of 2 or 3 in one or two attributes leaves room for other choices. Higher gates would force a single build.
- **Unavailable backgrounds are still shown,** with what they require, so new players learn what to spend Karma on. Spending happens before the background choice, so a player can go back and adjust.
- **Race gates allow cultural backgrounds,** such as a t'skrang River Pilot.

**Awakened backgrounds** require Magic 1 or more and grant the tradition's foundation casting skill, which is what Awakens the character. They grant no Magic themselves: a would-be caster pays for their talent with starting Karma. That's a path to playing a caster from the first minute, alongside the in-game paths in [Magic and Awakening](magic.md#how-to-awaken). They also grant fewer practical skills: an Acolyte starts with their faith, not a sword.

## Open questions

- [ ] **Do races adjust starting attributes,** or only grant racial skills? If they do, by how much? See the attribute leanings in [Races](#races).
- [ ] **Racial skill details:** are the proposed racial skills right for each race?
- [ ] **Which backgrounds,** and exactly what does each grant and require?
- [ ] **Are Awakened backgrounds balanced?** They cost at least 1 starting Karma (for Magic 1) and grant fewer practical skills. Is that enough, or should the Magic requirement be higher?
- [ ] **Universal starting skills:** basic attack only, or also a survival skill such as Foraging?
