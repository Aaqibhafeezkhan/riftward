# Magic and Awakening

[Design summary](README.md) · Status: designed, not built (Magic exists as an attribute and cannot be raised until Awakening)

Every character starts at Magic 0 and cannot learn magic skills until they Awaken, a one-time epiphany that sets Magic to 1. The path a character Awakens by gives them a tradition, which decides their magic trees and how they bear the cost of casting.

- **Magic skills require Magic 1 or more,** so a master cannot teach magic to someone who has not Awakened.
- **After Awakening, Magic rises with Karma** like any attribute (see [Karma](attributes.md#karma)). Magic skills link to Magic, so a spell's effectiveness is its skill level plus Magic.
- **Casting has a cost, Drain,** resisted by Willpower plus the tradition's attribute.
- **Awakening uses the hidden-skill machinery:** conditions, an optional trigger, and an epiphany message. See [Hidden skills](skills.md#hidden-skills).

## Traditions

| Tradition | Archetype | Self-taught path | Tradition attribute |
| --- | --- | --- | --- |
| Hermetic | Magician | **Study:** high Logic, a lore skill, and reading a grimoire in a particular library | Logic |
| Shamanic | Shaman, druid | **Communion:** high Intuition and nights alone in wild places until a spirit answers | Charisma |
| Divine | Cleric | **Devotion:** high Willpower and prayer at shrines until a power answers | Intuition |
| Adept | Monk, warrior-mystic | **Discipline:** high Willpower and Body, and pushing a physical skill to its limit | Body |

The tradition decides which magic trees open: adepts get physical-magic trees (enhanced reflexes, killing hands), while hermetics, shamans and clerics get spellcasting trees with different flavours.

## Paths that work for any tradition

- **Apprenticeship.** A master of a tradition can Awaken an apprentice into that tradition. The master must have mastered the tradition's foundation skill, the same mastery rule as teaching a loot skill. The apprentice meets reduced conditions and must accept; the rite takes place with both in the same room. There is no limit on apprentices: the time it takes to reach mastery is the limit.
- **Ordeal.** Surviving a near-death event with high Willpower. The tradition follows the character's strongest tradition attribute.

## Which paths are known

**Study is public; the rest are rumours.** The library and its grimoire are a known quest, slow but certain, so no one who wants to play a magic user is locked out. The other self-taught paths are hidden epiphanies that players piece together.

## Possible later additions

- **Essence:** dark pacts or iron grafts give power now and lower Magic for good.
- **Burnout:** very rare events reduce Magic.

## Open questions

- [ ] **Are the four traditions right?** Hermetic, Shamanic, Divine and Adept.
- [ ] **Born-Awakened sorcerers?** A small Luck-weighted chance at character creation, at the cost of starting some players ahead.
