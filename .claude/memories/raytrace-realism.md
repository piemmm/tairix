---
name: raytrace-realism
description: "lib/raytrace realism bar — no primitive-looking objects; nature is irregular, aged, decayed, and every generated object is modelled in detail"
metadata:
  node_type: memory
  type: feedback
  originSessionId: ec20fc95-2bfa-4454-a2c3-2f41afa7c0ac
  modified: 2026-10-05T13:32:15.343Z
---

In `lib/raytrace` (plans/RAYTRACE.md) the target is photographic realism with correct physics and good performance. No generated object may read as a plain primitive: not a capsule or tube with rounded ends, not a cone, not a perfect sphere.

- **Organic irregularity.** Nature is never perfect. A snowman is rolled snow: crooked, lumpy, balls of differing sizes, lumpy coal eyes. Bark is rough and ridged. Trunks are not cones or cylinders.
- **Entropy in landscapes.** Account for decay, disease, rot, weathering and drying.
- **Detail on everything.** For example, a broken stump shows fibrous splitting and splinters, and fungal growths have proper forms.
- **Bark matches its species.** Oak bark is deeply furrowed and must never read as smooth, on living or dead wood. Wood laid bare by sloughed bark is weathered, checked and grain-ridged, never a smooth plane.
- **Roots grow out of the trunk.** A trunk's foot swells outward toward each root, and each root curves up into the trunk along its lobe, as one continuous form wearing the same bark. Roots are never separate cones stuck onto a cylinder.
- **Windthrown roots tear.** A fallen tree's root plate is soil and stones bound by roots of many sizes, each visibly broken off with torn, splintered ends. It is never a ring of cones.
- **Dead wood is not white or green.** Sawn faces weather grey-brown, the cut bark ring and bark break-edges are dark, and moss comes with age in patches, not over the whole piece.

**Why:** The user explicitly rejected "incorrect realism", meaning objects built from idealised primitives. They raised it twice: first with the snowman and bark, then again with stumps and growths.

**How to apply:** Before landing any prototype or recipe, check its silhouette, its ends, its breaks and its surface against how the real object forms and ages. Model the mechanism (rolling, fracture along the grain, growth habit, decay), not a primitive stand-in. Verify by rendering a close-up crop.
