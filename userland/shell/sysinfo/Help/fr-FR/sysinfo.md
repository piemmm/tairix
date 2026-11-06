## NAME

sysinfo — interroger les informations système

## SYNOPSIS

`sysinfo <query>`

## DESCRIPTION

Émet une requête typée vers l'API d'information système et affiche la
réponse. TAIRiX n'a ni `/proc` ni `/sys` : cette commande est le visage
terminal de la même API versionnée et contrôlée par capacités que tout
programme utilise, et aucun chemin ne contourne le contrôle de
capacité.

Les requêtes :

- `processes`, `ps` — lister les processus, une ligne par processus.
- `memory`, `mem` — statistiques mémoire du noyau (nécessite
  `CAP_SYSINFO_KERNEL`).
- `hardware`, `hw` — l'arbre matériel détecté (nécessite
  `CAP_SYSINFO_HW`).
- `identity`, `id` — identité de la machine et version de l'OS.
- `uptime` — temps écoulé depuis le démarrage et heure du démarrage.
- `limits`, `rlimits` — vos limites de ressources effectives et leur
  usage en direct.
- `seats` — l'inventaire des sièges : le propriétaire de chaque écran
  et sa console de premier plan (nécessite `CAP_SYSINFO_HW`).
- `pressure` — la jauge de pression mémoire en direct : bande, seuils
  et compteurs de transition (nécessite `CAP_SYSINFO_KERNEL`).
- `reclaim` — le registre des caches récupérables, une ligne par classe
  (nécessite `CAP_SYSINFO_KERNEL`).
- `ramzip` — les compteurs du niveau de mémoire compressée (nécessite
  `CAP_SYSINFO_KERNEL`).
- `cpu` — profondeur de file, changements de contexte et préemptions
  par CPU (nécessite `CAP_SYSINFO_KERNEL`).
- `cpuinfo` — le rapport processeur par CPU (un surensemble de
  `/proc/cpuinfo`) : modèle/fabricant, classe de performance, indicateurs
  d'extensions ISA, le registre d'identité brut, la fréquence d'horloge
  de cœur mesurée en direct (en MHz — ou un « unknown » honnête là où
  aucun compteur d'horloge de cœur n'existe) et la fréquence de
  référence/base de temps fixe. Faits matériels publics, aucune
  capacité requise.
- `irq`, `irqs` — la table des IRQ du noyau : une ligne par ligne
  d'interruption associée — son identifiant, la tâche du pilote
  propriétaire, le nombre d'interruptions depuis le démarrage et si la
  ligne est en quarantaine (nécessite `CAP_SYSINFO_HW`).
- `frames` — ce qu'ont coûté les trames composées de chaque session de
  bureau : une ligne par session qui publie — les pixels que ses trames
  ont recomposés et la moyenne par trame, le multiple de surdessin
  (contributions de calques par pixel endommagé), la part résolue par la
  copie d'une plage opaque, les pixels qu'un fond en verre dépoli
  recalculé a de nouveau floutés, les pixels convertis vers le format de
  balayage, la pire trame isolée et sa part de l'écran, les appels au
  pilote qui les ont présentées, ainsi que les succès et échecs du cache
  des ornements de fenêtre. Ce sont les chiffres propres du bureau — seul
  un processus détenant un compositeur peut compter des pixels —, si bien
  que chaque ligne nomme l'éditeur tel que le service l'a attesté, et une
  session qui n'a rien publié n'affiche aucune ligne (nécessite
  `CAP_SYSINFO_GLOBAL`).
- `storage`, `io` — trois tables par volume, avec dans chacune une ligne
  par volume sur blocs conscient des pannes, repérée par un préfixe de
  l'identifiant durable du volume et nommant le périphérique qui le sert.
  Cette identité est le point d'accès du service de blocs qui le sert
  lorsqu'un pilote en espace utilisateur sert le périphérique, et une
  identité réservée pilotée par le noyau lorsque le noyau le pilote
  lui-même — le disque sur lequel la machine a démarré, qui n'a aucun
  point d'accès de service. **Service** nomme en outre le périphérique :
  le nom court que déclare son propre pilote (`virtio-blk`, `emmc2`,
  `usb-msd`), ou `-` s'il n'en déclare aucun. Cette table porte les octets
  cumulés, les requêtes achevées et le temps d'occupation du périphérique,
  qu'un lecteur convertit par différence en débit, IOPS, utilisation et
  attente — rien n'est précalculé, si bien que deux lecteurs n'héritent
  jamais d'une même fenêtre de moyenne (aucune capacité nécessaire).
  **File d'attente** porte ce qui est en cours maintenant, les
  accumulateurs dont on tire une profondeur moyenne par différence, ainsi
  que la profondeur et l'échéance que permet la classe propre du
  périphérique. **Santé** porte la disponibilité actuelle du volume
  (available/degraded/recovering/lost) et les compteurs de résultats
  cumulés (achèvements, réinitialisations, expirations, erreurs de
  support, réémissions) sur lesquels un disque défaillant ou instable
  devient visible. Les deux dernières nécessitent `CAP_SYSINFO_KERNEL` ;
  la table de service est affichée en premier, si bien qu'un appelant qui
  ne le détient pas lit encore les chiffres d'utilisation avant que le
  refus ne soit signalé.
- `raid`, `arrays` — les grappes RAID composées et les périphériques que
  détient le compositeur de grappes : une ligne par grappe — un préfixe
  de son identité, son niveau, sa santé
  (optimal/degraded/recovering/failed), le nombre de membres synchronisés
  et définis, son unité d'entrelacement, son nombre de blocs et toute
  reconstruction ou vérification en cours — puis une ligne par
  périphérique — son nœud de l'arbre matériel, la grappe à laquelle il
  appartient (un tiret pour un candidat non affilié), son emplacement,
  son rôle (candidate/held/in-sync/resyncing/faulted), sa taille et la
  génération de métadonnées qu'il porte (nécessite `CAP_SYSINFO_HW`).
- `dma`, `iommu` — les unités de traduction DMA : une ligne par unité — son
  nœud dans l'arbre matériel, sa famille, si elle traduit ou pourquoi non,
  comment elle signale ses fautes, où vivent ses traductions, les
  propriétaires et les flux du micrologiciel qu'elle détient, et ses fautes
  enregistrées et écartées, et les flux qu'une tempête a réduits au silence,
  depuis le démarrage — puis une ligne par groupe d'isolation qu'un
  propriétaire détient — son unité, le nœud et la génération du détenteur et
  où en est ce propriétaire — puis une ligne par nœud qu'une unité traduit
  pour un propriétaire — son unité et son groupe, la génération et l'état de
  son propriétaire, ses flux, et les mappages de son domaine et les octets
  qu'ils couvrent (requiert `CAP_SYSINFO_HW`).
- `audio`, `sound` — les périphériques audio : une ligne par sortie et par
  entrée — son identifiant, s'il est le choix par défaut de son sens, son
  niveau et sa mise en sourdine, la fréquence mesurée de son horloge pendant
  la lecture, les trames perdues, son emplacement et son nom — puis une
  ligne par flux audio qui vous appartient — son périphérique, son sens, son
  rôle et son état, sa position, ses défauts et qui le détient.
- `show <resource-ref>` — lit une référence de ressource
  `info:`/`state:`/`stats:` et affiche sa valeur. Ces espaces de noms
  fournissent des valeurs typées via cette API, jamais des flux d'octets :
  `cat` ne peut pas les ouvrir. Un refus nomme la capacité requise.
- `describe <resource-ref>` — affiche l'enveloppe de la réponse au lieu de
  la valeur : son producteur, l'autorisation sous laquelle elle a été
  servie, et les métadonnées de la charge utile — pour une métrique son
  genre, son unité, son comportement de remise à zéro et sa fenêtre
  d'échantillonnage ; pour un fait son type et sa sensibilité.
- `help` — l'aide courte de cette commande.

Sans requête, l'aide courte est affichée.

## OPTIONS

- `--all, -a` — avec `processes` : lister tous les processus du système
  plutôt que seulement les vôtres ; le service n'accorde cette vue qu'à
  un appelant détenant `CAP_SYSINFO_GLOBAL`.
  Avec `audio`, tous les flux audio de la machine, sous la même capacité.
- `-h, -?` — afficher l'aide courte de cette commande.

## EXAMPLES

- `sysinfo identity` — afficher l'identité de la machine et la version
  de l'OS.
- `sysinfo ps --all` — lister tous les processus du système.
- `sysinfo frames` — montrer ce que coûtent les trames du bureau.

## EXIT STATUS

- `0` — la requête a été répondue et affichée.
- `1` — le service a refusé ou échoué, ou le résultat n'a pas pu être
  délivré. Une ligne de commande non comprise sort aussi avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`).

## SEE ALSO

- `man`
- `ps`
- `top`
