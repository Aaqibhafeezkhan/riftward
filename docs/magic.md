# Magic and Awakening

[Design summary](README.md) · Status: Awakening is built (Magic as latent talent, a teacher for each tradition, foundation skills), and so are spells: `cast`, Drain, and the first two spells of each casting tradition, and Hermetic's Kindle Blade

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

Being Awakened shows in `score` ("Magic 1 (Awakened: Divine)"). The foundation skills exist and Awaken, and are each tradition's first school: see [Spells](#spells).

## Traditions

| Tradition | Archetype | Self-taught path | Tradition attribute |
| --- | --- | --- | --- |
| Hermetic | Magician | **Study:** high Logic, a lore skill, and reading a grimoire in a particular library | Logic |
| Shamanic | Shaman, druid | **Communion:** high Intuition and nights alone in wild places until a spirit answers | Charisma |
| Divine | Cleric | **Devotion:** high Willpower and prayer at shrines until a power answers | Intuition |
| Adept | Monk, warrior-mystic | **Discipline:** high Willpower and Body, and pushing a physical skill to its limit | Body |

The tradition decides which magic opens: hermetics, shamans and clerics get schools and spells with different flavours, while adepts turn magic inward and have **powers** instead of spells, which are ordinary skills in their tree (Killing Hands, Quickened Reflexes). Self-taught paths can also test latent Magic, for example Study requiring Magic 2, so early investment makes Awakening easier.

**Inner Discipline is the Adept's school.** A caster's foundation grows by casting and powers their spells; the Adept's foundation does the same for powers. *Built:*

- **It grows with the body's work:** every use of an Adept power practises it (the `UsedTree` trigger), and so does every attack made with an attack skill, armed or not (`UsedAttack`): a swordsman's swing and a monk's punch alike. That's how a newly Awakened Adept, who knows no powers yet, starts on the path, whatever they fight with (2026-10-27; it was Brawling alone). The pace is worth watching: attacks come every round, though the gain cooldown limits how fast it can level.
- **It's worth having in itself:** a tenth of a point of attack with every level, armed or not, and a point of stamina (breath), so every level counts (a fifth of a point until 2026-10-30, when maximum health and stamina stopped counting fractions). Killing Hands gets more on top: +1 attack per 5 levels and +1 damage per 10. Before 2026-10-27 it gave bonuses only to Killing Hands, so an Adept who fought with a sword got nothing from it but the tree; see [Every skill is worth having for itself](skills.md#skill-trees-and-skills).
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

- **A spell belongs to a school** and has a minimum school level, a Drain cost, an aim, what resists it, and its effects. It never levels.
- **Learning a spell** happens once: from a tradition's teacher (built), a scroll (used up) or a grimoire (studied, kept). Finding a new spell should feel like gaining a new option, not starting a new climb from level 0. Learning costs no Karma, but a teacher asks money for every spell but the tradition's starter (Mend, Spark, Spirit Grasp); see [Lessons](economy.md#lessons).
- **Casting** is an action like any other: `cast <spell> [<target>]` takes a round. Spells are cast, not used, but `use <spell>` finds them too and casts them, so players needn't remember which is which (2026-10-26). Its power is the school's rating, **school level + the school's attribute + Magic**, contested by the ratio rule like everything else, and casting practises the school. Every spell in a school grows stronger together, so a caster's progress isn't split between a dozen spells.
- **Drain** is what a spell costs: stamina, resisted by Willpower plus the school's attribute against twice the Drain, as a fixed difficulty. Resisted, it costs half (rounded up); a failed resistance costs it all ("The casting takes more out of you than it should."). A caster without the stamina for the whole Drain can't cast. Great spells costing health are for later.
- **Spells don't add to character level or Karma** beyond what practising their school does, so casters don't out-level fighters by knowing more.
- **How many spells a caster can hold** has no limit for now; it's decided once there are more than two spells per tradition (an open question below).
- **Data:** spells live in `assets/spells/<tradition>.spells.ron` (key `tradition:spell`), hot-reloaded and checked at load like skills; each character's known spells in the `character_spells` table. `spells` lists what a character knows, and `spells <spell>` reads about one.

```ron
"spark": (
    name: "Spark",
    description: "Loose a crackling bolt of force at a foe...",
    school: "hermetic:foundation",
    min_level: 0,                     // the school level it needs
    drain: 2,                         // stamina; half if resisted
    aim: Foe,                         // Foe, Ally or Other
    resist: Some(Dodge),              // or Some(Attribute(Willpower)); none for a blessing
    effects: [Damage(amount: "roll(2, 5) + floor(power / 3)", type: "force")],  // a formula; see effects.md
    text: (hit: (...), miss: (...)),  // like a skill's, with {harm} when it harms
),
```

### How a spell is cast (built)

- **Aim** says whom a spell goes to, and whether it starts a fight:
  - **Foe:** hostile. Your foe unless you name another; never another player. Cast out of a fight, it starts one and is cast on your first turn, the way an opener is; a foe it's cast at fights back.
  - **Ally:** yourself unless you name someone else here. Cast now, in a fight or out of one.
  - **Other:** someone else here, your foe by default, without starting a fight (Beast Speech).
- **Resisting:** each spell names what resists it. **Dodge** is Reaction plus Dodge, as against a blow, but a shield doesn't help against magic; an **attribute** is the target's rating in it. A spell aimed at a foe must be resistable. Unresisted spells (blessings, healing) always work.
- **Effects** (any number, in the [shared vocabulary](effects.md#effects-as-built); amounts are [formulas](effects.md#formulas-as-built), and the first spells add a third of the power to theirs):
  - **Damage:** of a type, landing harder the more decisively the spell won (the same [blows](attributes.md#the-ratio-rule) as an attack), through armour and resistances; told in Diku's words with `{harm}`.
  - **Heal:** health back, up to the most.
  - **Apply** a condition: Bless puts on *blessed* (attacks better), Ward *warded* (takes the next blows' damage in their place, up to its strength, from any blow, weapon or spell; its strength is `4 + floor(power / 3)`, so it grows with the power), Spirit Grasp *held* (can't flee, defends at half, as a grapple leaves). **Remove** and **Cure** take conditions off.
  - **Calm:** an animal (anything that fights with the Beast tree) stops fighting, no one fights it, and it no longer attacks those who come near.
- **In a fight,** a queued `cast` is cast on the caster's turn in the round, in initiative order like any blow, so its damage, deaths and experience go the way a blow's do: `src/combat/casting.rs` pays for it, and `src/combat/resolve.rs` resolves it like any blow.
- **Mobs cast** by the same rules. A template lists `spells` (and the school skill and attributes they need); on its turn, with its `tactics` chance, a mob picks among its techniques and the spells that would do some good: a spell at its foe, healing itself only when badly hurt, blessing or warding itself only when it isn't already. The three teachers know their tradition's two spells and cast them if attacked.
- **Lasting effects count down in time, fighting or not,** so a blessing can't be cast, rested on to win back the stamina, and saved for a fight. Each spell says how long its effect lasts in game time (`lasts: Minutes(80)`, `Hours(12)`; a game hour is 7.5 real minutes), and the bearer is told when it fades. `affects` (`aff`) lists everything on you, what it does and how long it has left, in game time and real ("blessed: attacking 2 better, for 1 hour 19 minutes (10 real minutes)"); `score` names them. Durations differ by spell: Bless and Ward last 80 minutes (10 real minutes); an Armour spell, when there is one, might last 12 hours (an hour and a half). A technique's conditions (staggered, held and the rest) still count the bearer's turns, and so does Spirit Grasp's hold.
- **Later skills can stretch durations or cut Drain:** a caster's lasting effects are timed in one place (`combat/effects.rs`), which such skills would add to, and Drain is resisted in one place (`spells.rs`).

### The first spells (built)

| Tradition | Spell | Aim | Resisted by | Does | Drain |
| --- | --- | --- | --- | --- | --- |
| Divine | **Mend** | Ally | | Heals 2 to 5 | 3 |
| Divine | **Bless** | Ally | | Attack +2 for 80 minutes | 2 |
| Hermetic | **Spark** | Foe | Dodge | 2 to 5 damage | 2 |
| Hermetic | **Ward** | Ally | | Takes 4 damage, within 80 minutes | 3 |
| Hermetic | **Kindle Blade** | Ally | | The weapon in hand burns for 1 to 3 more fire damage a blow, for 10 blows or 80 minutes (Hermetic Principles 5) | 3 |
| Shamanic | **Beast Speech** | Other | Willpower | Calms an animal | 2 |
| Shamanic | **Spirit Grasp** | Foe | Strength | Held for 2 turns | 3 |

All are cast through the tradition's foundation, at level 0 but for Kindle Blade at 5, and taught by its teacher (`learn`), who needs the student to know the school first.

## Possible later additions

- **Armour and Shield spells,** lasting much longer than a ward: Armour, say, for 12 game hours.
- **Skills that stretch spells' durations or cut their Drain,** in each tradition's craft of casting (Litany already sustains blessings).

- **Essence:** dark pacts or iron grafts give power now and lower Magic for good.
- **Burnout:** very rare events reduce Magic.

## Open questions

- [ ] **Are the four traditions right?** Hermetic, Shamanic, Divine and Adept.
- [ ] **Can a character know more than one foundation skill?** That would mean belonging to two traditions.
- [ ] **How many spells can a caster hold?** No limit for now (2026-10-26), decided once there are more spells. Options: a number from school levels (say one per 10 levels in each school), from an attribute (Logic for Hermetics, Intuition for Shamans), or a grimoire that holds them and must be carried.
- [x] **Do spells cost Karma to learn?** No: only the effort of finding them (2026-10-26).
