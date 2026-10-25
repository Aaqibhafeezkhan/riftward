# Magic and Awakening

[Design summary](README.md) · Status: Awakening is built (Magic as latent talent, a teacher for each tradition, foundation skills); casting is not

Every character starts at Magic 0 and can raise it with Karma from creation onward. On its own, Magic is **latent talent**: it does nothing until the character knows a **casting skill**, a tradition's foundation skill. Knowing one is what it means to be Awakened, and which one decides the character's tradition.

- **Awakening is part of the skill system.** There is no separate Awakened state: a character is Awakened when they know a foundation skill, so the usual routes for acquiring skills (starting skills, teaching, epiphany) are the routes to Awakening.
- **Foundation skills require Magic 1 or more.** Latent talent is what makes a character eligible to Awaken later; spending Karma on Magic before Awakening is an investment, not a waste.
- **A tradition's other skills are its schools,** which require its foundation. Schools level by use like any skill.
- **Spells are not skills.** A spell is something a caster *knows*, learned once and cast through a school; see [Spells](#spells).
- **Casting has a cost, Drain,** resisted by Willpower plus the tradition's attribute.

## How to Awaken

Every character starts mundane. Awakening happens in play, like learning a sword:

1. **Find a tradition's teacher.** Each tradition has one in the world, who teaches its foundation skill to anyone with latent talent (Magic 1 or more), the same way the drill sergeant teaches Swordplay (`learn`, see [Trainers](skills.md#acquiring-skills)). *Built:*

   | Tradition | Teacher | Where |
   | --- | --- | --- |
   | Divine | The keeper of the shrine | Riftwatch, the Wayside Shrine |
   | Hermetic | The magister | Riftwatch, the Guild Archive |
   | Shamanic | The hedge-shaman | The Darkwood, the Hermit's Clearing north of the North Road |
   | Adept | An old ascetic | Riftwatch, the Watchtower |

2. **A tradition's self-taught path:** the foundation skill is a [hidden skill](skills.md#hidden-skills) learned by epiphany when the path's conditions are met. See the table below. Rarer and slower, for those who find it.
3. **Apprenticeship to a player:** a master teaches the foundation skill. They must have mastered it, the same rule as teaching a loot skill. The apprentice needs only the skill's requirements, starting with Magic 1.
4. **Ordeal:** surviving a near-death event with high Willpower and latent Magic. The epiphany grants the foundation skill of the tradition matching the character's strongest tradition attribute.

Being Awakened shows in `score` ("Magic 1 (Awakened: Divine)"). The foundation skills exist and Awaken; casting, Drain and spells are still to come.

## Traditions

| Tradition | Archetype | Self-taught path | Tradition attribute |
| --- | --- | --- | --- |
| Hermetic | Magician | **Study:** high Logic, a lore skill, and reading a grimoire in a particular library | Logic |
| Shamanic | Shaman, druid | **Communion:** high Intuition and nights alone in wild places until a spirit answers | Charisma |
| Divine | Cleric | **Devotion:** high Willpower and prayer at shrines until a power answers | Intuition |
| Adept | Monk, warrior-mystic | **Discipline:** high Willpower and Body, and pushing a physical skill to its limit | Body |

The tradition decides which magic opens: hermetics, shamans and clerics get schools and spells with different flavours, while adepts turn magic inward and have **powers** instead of spells, which are ordinary skills in their tree (Killing Hands, Quickened Reflexes). Self-taught paths can also test latent Magic, for example Study requiring Magic 2, so early investment makes Awakening easier.

**Inner Discipline is the Adept's school.** A caster's foundation grows by casting and powers their spells; the Adept's foundation does the same for powers. *Built:*

- **It grows with the body's work:** every use of an Adept power practises it (the `UsedTree` trigger), and so does fighting unarmed with Brawling (`UsedSkill("brawling:basics")`), which is how a newly Awakened Adept, who knows no powers yet, starts on the path.
- **It strengthens the powers:** +1 attack with Killing Hands per 5 levels, and +1 damage per 10.
- **It opens the tree:** Killing Hands (Inner Discipline 0) and Quickened Reflexes (5) are [discovered](skills.md#discovery) while practising it, like any tree's higher skills. The ascetic teaches only Inner Discipline.

## Which paths are known

**Teachers are the open door; self-taught paths are rumours.** Anyone with talent who finds a teacher can Awaken, so no one who wants to play a magic user is locked out; the hedge-shaman takes a walk into the Darkwood to find. Study at the Guild Archive is the best known self-taught path; the others are hidden epiphanies that players piece together.

## Spells

Spells are their own category, separate from skills. **Schools are what improve; spells are what you know.**

- **Schools are skills.** Each casting tradition has a few schools, ordinary skills in its tree that level by use. The foundation is the first school; others open later, each requiring the foundation. A first pass:

  | Tradition | Schools |
  | --- | --- |
  | Divine | **Faith** (the foundation: blessings and mending), **Wrath** (smiting), **Warding** |
  | Hermetic | **Hermetic Principles** (the foundation: force and light), **Evocation**, **Abjuration**, **Divination** |
  | Shamanic | **Spirit Sight** (the foundation: speaking with spirits), **Beasts**, **Elements**, **Ancestors** |

- **The rest of a tradition's tree is the craft of casting:** Drain resistance, shaping, countering, sustaining, summoning, rituals and a capstone, all skills worth improving. See [Skill trees](trees.md#magic) for each tradition's.

- **A spell belongs to a school** and has a minimum school level, a Drain cost, a target and an effect. It never levels.
- **Learning a spell** happens once: from a tradition's teacher, a scroll (used up) or a grimoire (studied, kept). Finding a new spell should feel like gaining a new option, not starting a new climb from level 0.
- **Casting** is an action like any other: `cast <spell> [<target>]` takes a round. Its power is **school level + Magic**, contested by the ratio rule like everything else, and casting practises the school. Every spell in a school grows stronger together, so a caster's progress isn't split between a dozen spells.
- **Drain** is what a spell costs: stamina (and, for great spells, health), resisted by Willpower plus the tradition's attribute. A failed resistance costs more.
- **Spells don't add to character level or Karma.** Only skills do, so casters don't out-level fighters by knowing more.
- **How many spells a caster can hold** is limited, so they choose a repertoire rather than collecting everything (an open question below).
- **Data:** spells live in `assets/spells/<tradition>.spells.ron`; each character's known spells in a `character_spells` table. `spells` lists what a character knows.

```ron
"mend": (
    name: "Mend",
    school: "divine:foundation",
    min_level: 0,
    drain: Stamina(2),
    target: Touch,                  // self or someone here
    effect: Heal(dice: (2, 5)),     // plus a share of school level + Magic
),
```

## Possible later additions

- **Essence:** dark pacts or iron grafts give power now and lower Magic for good.
- **Burnout:** very rare events reduce Magic.

## Open questions

- [ ] **Are the four traditions right?** Hermetic, Shamanic, Divine and Adept.
- [ ] **Can a character know more than one foundation skill?** That would mean belonging to two traditions.
- [ ] **How many spells can a caster hold?** Options: a number from school levels (say one per 10 levels in each school), from an attribute (Logic for Hermetics, Intuition for Shamans), or a grimoire that holds them and must be carried.
- [ ] **Do spells cost Karma to learn,** as in Shadowrun, or only the effort of finding them?
