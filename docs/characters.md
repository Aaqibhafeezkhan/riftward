# Races, backgrounds and character creation

[Design summary](README.md) · Status: built. Starting skills are granted for the skills that exist as data so far

A new character's starting point comes from two choices: their **race** and their **background**. There are still no classes. Race and background only decide where a character starts; from there, they learn and use whatever skills they like.

## Character creation

1. **Name** the character.
2. **Choose a race.** It shifts the base attributes and grants racial skills.
3. **Spend starting Karma:** `raise <attribute>`, `lower <attribute>` to take a point back, then `done`. Karma spent on Magic is latent talent: it does nothing until the character knows a casting skill. Unspent Karma carries into play.
4. **Choose a background.** It grants a few starting skills. Backgrounds the character doesn't qualify for are shown dimmed, with what they require, and refused with an explanation.
5. **Describe** the character as others see them, or `skip`. `describe <text>` changes it later, and `look <player>` shows it.

Every step after the name can go `back`, so a player can choose a background, find it needs Agility 4, and go back to buy it. Nothing but the name is saved until the last step: a character left half-made goes through creation again at its next login. So do characters made before races existed.

Every character will also get the universal starting skills, such as a basic attack, so everyone can fight and survive from the first minute. Skills granted at creation are recorded with `learned_via: Starting` and start at level 0, then improve through use like any other skill.

### When a race or background is removed

Races and backgrounds are data, and a builder may remove one that characters already have. Those characters aren't lost; they're stopped at character selection and choose again before they can play:

- **A removed race:** "Grok's race, 'gnome', no longer exists in this world." They choose a new race and spend their starting Karma afresh from its starting ratings, since the old race's are gone. Their background must be chosen again too, as the new attributes may not meet its gate. Their description can be kept.
- **A removed background:** they choose a new one; race, attributes and description stay. They can go `back` to re-spend their starting Karma on the way, a free respec that only happens when an admin removes something.

## Races

The eight races of Earthdawn, in `assets/races.ron`. Each shifts the base attributes, Earthdawn-style, and grants racial skills once skills exist. Every race's shifts plus its extra Karma total **+3**, so no race starts ahead overall; humans get their +3 as Karma to spend as they like instead.

Attributes start from a base of **3** (Luck 1, Magic 0), so races have room to be weak as well as strong. No shift takes an attribute below 1.

| Race | In brief | Shifts | Starts | Racial skills (proposed) |
| --- | --- | --- | --- | --- |
| Dwarf | Stout, stubborn crafters and miners | +2 Body, +1 Strength, +1 Willpower, −1 Reaction | Body 5, Reaction 2 | **Heat Sight** (passive): see warm bodies in darkness |
| Elf | Graceful, long-lived, close to the living world | +2 Agility, +1 Intuition, +1 Charisma, −1 Body | Agility 5, Body 2 | **Low-Light Vision** (passive) |
| Human | Adaptable, found everywhere | +3 Karma | All 3, 13 Karma | **Versatility** (passive): practice builds faster |
| Obsidiman | Living stone, huge, patient and slow | +3 Body, +3 Strength, −1 Agility, −2 Reaction | Body 6, Strength 6, Reaction 1 | **Stone Skin** (passive): natural armour |
| Ork | Fierce, hardy, quick to anger | +2 Strength, +1 Body, +1 Reaction, −1 Charisma | Strength 5, Charisma 2 | **Low-Light Vision** (passive); **Gahad** (active): a burst of rage |
| Troll | Towering, proud, warrior culture | +3 Strength, +2 Body, −1 Agility, −1 Logic | Strength 6, Body 5 | **Heat Sight** (passive) |
| T'skrang | Flamboyant lizard-folk of the rivers | +2 Charisma, +1 Agility, +1 Reaction, −1 Willpower | Charisma 5 | **Tail Combat** (active); **Swimming** (passive) |
| Windling | Tiny, winged, curious | +3 Agility, +1 Reaction, +1 Intuition, −2 Strength | Agility 6, Strength 1 | **Flight** (passive); **Astral Sight** (active) |

```ron
(
    id: "troll",
    name: "Troll",
    summary: "Towering and proud, of a warrior culture.",
    attributes: {Strength: 3, Body: 2, Agility: -1, Logic: -1},
    skills: ["racial:heat_sight"],
),
```

- **Karma counts from where a race starts.** A troll's first six points of Strength weren't bought, so they cost nothing; `lower` can't take an attribute below its race's start.
- **Size matters later.** Windlings can't wield large weapons, and trolls and obsidimen need bigger gear. That belongs to the item system.
- **Racial abilities are ordinary skills that require the race,** using a `Race` condition. They can be taught, but only to students of the same race.

## Backgrounds

A background is a life before play: a few starting skills and, for some, a magical tradition. They're in `assets/backgrounds.ron`, in menu order. Any background can be gated by a `requires` condition, in the shared [condition language](#conditions). With attributes starting at 3, a gate of 4 is one point bought with Karma, unless the character's race already gives it.

| Background | Requires | Starting skills (names to tune) |
| --- | --- | --- |
| Soldier | Body 4 | Swordplay basics, Toughness |
| Poacher | Agility 4 | Archery basics, Stealth basics |
| Street Urchin | Nothing | Stealth basics, Pickpocketing, Lockpicking |
| Farmhand | Nothing | Foraging, Brawling basics, Animal handling |
| Sailor | Strength 4 | Swimming, Climbing, Knife basics |
| Merchant's Child | Charisma 4 | Haggling, Appraisal |
| Smith's Apprentice | Strength 4 | Smithing, Hammer basics |
| Herbalist's Apprentice | Intuition 4 | Herbalism, First aid |
| Minstrel | Charisma 4 | Performance, Lore |
| Scholar | Logic 4 | Lore, Scribing |
| River Pilot | T'skrang | Sailing, Swimming |
| Acolyte (Awakened: Divine) | Magic 1 and Willpower 4 | The Divine foundation skill |
| Mystic (Awakened: Shamanic) | Magic 1 and Intuition 4 | The Shamanic foundation skill |
| Apprentice (Awakened: Hermetic) | Magic 1 and Logic 4 | The Hermetic foundation skill |
| Adept (Awakened: Adept) | Magic 1 and Body 4 | The Adept foundation skill |

```ron
(
    id: "acolyte",
    name: "Acolyte",
    summary: "You served a temple, and your faith answered. (Awakened: Divine)",
    requires: All([Stat(Magic, 1), Stat(Willpower, 4)]),
    skills: ["divine:foundation"],
),
```

- **Gates follow what a background implies,** and must be affordable from starting Karma. A test checks that every background is open to at least one race spending its starting Karma on the gate.
- **Two backgrounds have no gate,** Street Urchin and Farmhand, so every build has somewhere to start.
- **Race gates allow cultural backgrounds,** such as the t'skrang River Pilot.
- **Awakened backgrounds** require Magic 1 and grant the tradition's foundation casting skill, which is what Awakens the character. They grant no Magic themselves: a would-be caster pays for their talent with starting Karma, and starts with their faith or craft rather than a sword.

## Conditions

The shared language for "who qualifies" (`src/conditions.rs`). Backgrounds use it now; skill requirements, hidden-skill unlocks, quests and trainers will add terms to it.

| Condition | Met when |
| --- | --- |
| `Stat(Attribute, min)` | The attribute is at least `min` |
| `Race(id)` | The character is of that race |
| `All([...])`, `Any([...])`, `Not(...)` | Combinations |

Players read them as "Magic 1 and Willpower 4", or "T'skrang".

## Open questions

- [ ] **Racial skill details:** are the proposed racial skills right for each race?
- [ ] **Exact skill grants** for each background, once skill trees exist.
- [ ] **Are Awakened backgrounds balanced?** They cost at least 1 starting Karma (for Magic 1) and grant fewer practical skills.
- [ ] **Universal starting skills:** basic attack only, or also a survival skill such as Foraging?
- [ ] **Starting equipment by background:** an urchin's lockpicks, a soldier's sword (see [Items](items.md#open-questions)).
