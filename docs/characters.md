# Races, backgrounds and character creation

[Design summary](README.md) · Status: built. Starting skills are granted for the skills that exist as data so far

A new character's starting point comes from two choices: their **race** and their **background**. There are still no classes. Race and background only decide where a character starts; from there, they learn and use whatever skills they like.

## Character creation

1. **Name** the character.
2. **Choose a race.** It shifts the base attributes and grants racial skills and traits.
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

Ten races, in `assets/races.ron` ([The setting](setting.md#the-peoples)). Each shifts the base attributes and grants racial skills and traits (what every one of the race simply is, like an elf's Fey Mind: see [Traits](effects.md#traits)). Every race's shifts net **+3** points; humans get no shifts and **12 Karma** to spend as they like instead, and **Versatility**, which makes their skills grow faster. A test checks every race keeps to this.

Since a point costs the rating it reaches, shifts aren't all worth the same: a third point of Agility (5 to 6) is worth 6 Karma, while a lost point low down (3 to 2) saves only 3. At real prices the races' shifts are worth:

| Race | Shifts worth |
| --- | --- |
| Halfling | 12 Karma (a point of Luck is cheap: 1 to 2 costs 2) |
| Dwarf, Elf, Kitsune, Lizardfolk, Ork | 14 Karma |
| Troll, Sprite | 18 Karma |
| Stoneborn | 22 Karma |
| Human | none: 12 Karma instead, and Versatility |

**This is left uneven on purpose for now.** A race is a whole package (shifts, racial skills and any extra Karma), and racial skills and traits are only begun (the elves' Fey Mind, resisting sleep and charm, is the first, a trait since 2026-10-27): a sprite's Flight and Astral Sight, an ork's Gahad, a human's Versatility are still to come. Each race gets balanced once against its whole package when they are, so a big shift like the stoneborn's can be paid for with fewer or smaller racial skills. Humans don't open trees without a trainer: Versatility, learning faster, is their edge (2026-10-26).

Attributes start from a base of **3** (Luck 1, Magic 0), so races have room to be weak as well as strong. No shift takes an attribute below 1.

| Race | In brief | Shifts | Starts | Racial skills (proposed) |
| --- | --- | --- | --- | --- |
| Dwarf | Stout, stubborn crafters and miners | +2 Body, +1 Strength, +1 Willpower, −1 Reaction | Body 5, Reaction 2 | **Heat Sight** (a racial trait, *built*): sees in the dark ([Light and darkness](world.md#light-and-darkness)) |
| Elf | Graceful, long-lived, close to the living world | +2 Agility, +1 Intuition, +1 Charisma, −1 Body | Agility 5, Body 2 | **Low-Light Vision** (passive), **Fey Mind** (a trait, built: resists sleep and charm by 4) |
| Halfling | Small, nimble and famously lucky | +2 Agility, +1 Charisma, +1 Luck, −1 Strength | Agility 5, Luck 2, Strength 2 | **Small and Quick** (passive): hard to pin down |
| Human | Adaptable, found everywhere | +12 Karma | All 3, 32 Karma | **Versatility** (passive): practice builds faster |
| Kitsune | Fox-folk of fey blood, clever and hard to fool | +2 Charisma, +1 Agility, +1 Intuition, −1 Strength | Charisma 5, Strength 2 | **Fox Form** (active): become a fox for a while, quick and stealthy but unable to speak or cast; **Foxfire** (active): wisps of flame, more as they grow; **Fey Mind** (the elves' trait, built: resists sleep and charm) |
| Lizardfolk | Flamboyant reptilian folk of the rivers | +2 Charisma, +1 Agility, +1 Reaction, −1 Willpower | Charisma 5 | **Tail Combat** (active); **Swimming** (passive) |
| Ork | Fierce, hardy, quick to anger | +2 Strength, +1 Body, +1 Reaction, −1 Charisma | Strength 5, Charisma 2 | **Low-Light Vision** (passive); **Gahad** (active): a burst of rage |
| Sprite | Tiny, winged, curious | +3 Agility, +1 Reaction, +1 Intuition, −2 Strength | Agility 6, Strength 1 | **Flight** (passive); **Astral Sight** (active) |
| Stoneborn | Living stone, huge, patient and slow | +3 Body, +3 Strength, −1 Agility, −2 Reaction | Body 6, Strength 6, Reaction 1 | **Stone Skin** (passive): natural armour |
| Troll | Towering, proud, warrior culture | +3 Strength, +2 Body, −1 Agility, −1 Logic | Strength 6, Body 5 | **Heat Sight** (a racial trait, *built*) |

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
- **Size matters later.** Sprites can't wield large weapons, halflings find some unwieldy, and trolls and stoneborn need bigger gear. That belongs to the item system.
- **Racial abilities are ordinary skills that require the race,** using a `Race` condition. They can be taught, but only to students of the same race.

## Backgrounds

A background is **what a character did before they were an adventurer**: a little practice in the everyday skills of that life, and its tools. It never teaches fighting or magic. Every adventurer learns those the same way, from [trainers](skills.md#acquiring-skills) in the world, so a farmhand can become a swordsman and a soldier a shaman; the past colours a character without deciding what they become. That keeps backgrounds from turning into classes.

They're in `assets/backgrounds.ron`, in menu order. Any background can be gated by a `requires` condition, in the shared [requirement language](#conditions). With attributes starting at 3, a gate of 4 is one point bought with Karma, unless the character's race already gives it.

| Background | Requires | Everyday skills | Kit |
| --- | --- | --- | --- |
| Soldier | Body 4 | Toughness, First Aid | A short sword, two bandages |
| Poacher | Agility 4 | Stealth, Tracking | A knife |
| Street Urchin | Nothing | Lockpicking, Pickpocketing, Stealth | A knife, lockpicks |
| Farmhand | Nothing | Foraging, Animal Handling | A hatchet |
| Sailor | Strength 4 | Swimming, Climbing | A knife |
| Merchant's Child | Charisma 4 | Haggling, Appraisal | A knife |
| Smith's Apprentice | Strength 4 | Smithing | A smith's hammer |
| Herbalist's Apprentice | Intuition 4 | Herbalism, First Aid | A knife, two bandages |
| Minstrel | Charisma 4 | Performance, Lore | A knife |
| Scholar | Logic 4 | Lore, Scribing | A quarterstaff |
| River Pilot | Lizardfolk | Sailing, Swimming | A knife |

```ron
(
    id: "urchin",
    name: "Street Urchin",
    summary: "You grew up on the streets, living on what you could take.",
    skills: ["thievery:lockpicking", "thievery:pickpocketing", "stealth:basics"],
    items: ["riftwatch:knife", "riftwatch:lockpicks"],
),
```

- **Everyday skills, a head start.** They're skills anyone can learn later; a background only means starting with them. Ones not yet written as data are granted when they are. A test checks no background grants a weapon skill or a tradition's foundation.
- **The kit is the tools of that life,** on top of everyone's plain clothes and belt pouch. A weapon in the kit is usable untrained, with Brawling's aim and the weapon's damage (see [Combat](architecture.md#combat)), until its owner takes a lesson. The kit is given once, when a character is first made: worn if it's worn, in hand while hands are free, otherwise in the belt pouch.
- **Gates follow what a background implies,** and must be affordable from starting Karma. A test checks every background is open to at least one race spending its starting Karma on the gate.
- **Two backgrounds have no gate,** Street Urchin and Farmhand, so every build has somewhere to start.
- **Race gates allow cultural backgrounds,** such as the lizardfolk River Pilot.
- **No background is magical.** Everyone starts mundane; Magic bought at creation is latent talent, and a character is Awakened in play by finding a tradition's teacher (see [Magic](magic.md#how-to-awaken)).

## Conditions

The shared language for "who qualifies" (`src/conditions.rs`). Backgrounds use it now; skill requirements, hidden-skill unlocks, quests and trainers will add terms to it.

| Condition | Met when |
| --- | --- |
| `Stat(Attribute, min)` | The attribute is at least `min` |
| `Race(id)` | The character is of that race |
| `All([...])`, `Any([...])`, `Not(...)` | Combinations |

Players read them as "Magic 1 and Willpower 4", or "Lizardfolk".

## Open questions

- [ ] **Racial skill details:** are the proposed racial skills right for each race?
- [ ] **Exact skill grants** for each background, once skill trees exist.
- [x] **The Adept's weapon:** settled by **Killing Hands**, unarmed strikes for 2–4, discovered by Adepts through their tradition once Awakened (see [Skill trees](trees.md#step-1-as-built)).
- [ ] **Universal starting skills:** basic attack only, or also a survival skill such as Foraging?
- [x] **Starting equipment by background:** built; see [Backgrounds](#backgrounds).
