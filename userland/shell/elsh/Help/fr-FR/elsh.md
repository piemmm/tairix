## NAME

elsh — le shell de commandes de TAIRiX

## SYNOPSIS

`elsh [-h | -?]`

## DESCRIPTION

Lance un shell de commandes interactif — une boucle
lecture-évaluation-affichage sur les flux standard hérités. Un mot de
commande tapé est résolu d'abord parmi les commandes intégrées du
shell, puis dans le magasin de commandes système (`/System/Commands`),
le magasin d'applications système (`/System/Applications`), le magasin
de commandes (`<home>/Commands`) et le magasin d'applications
(`<home>/Applications`) propres à l'utilisateur, puis dans les
répertoires de la variable `PATH` ; ces quatre magasins forment un
préfixe fixe que l'utilisateur ne peut ni réordonner ni remplacer, donc
`PATH` ne peut jamais masquer une commande système. Un mot non résolu
sort avec `127` ; un paquet résolu mais non exécutable sort avec `126`.

Les commandes intégrées :

- `cd <path>`, `pwd` — changer et afficher le répertoire de travail.
- `echo ...` — afficher ses opérandes.
- `export NAME=value`, `unset NAME` — modifier l'environnement exporté.
- `jobs`, `fg`, `bg` — contrôle des tâches.
- `ulimit` — lire et imposer des limites de ressources.
- `elevate` — exécuter une commande ré-authentifiée via le superviseur
  de connexion de la console.
- `shopt [-pqsu] [-o] [name ...]` — afficher ou régler les options des
  motifs de noms de fichiers.
- `help` — lister les commandes intégrées.
- `exit [code]` — terminer la session.

Un mot contenant un `*`, un `?` ou un `[...]` non protégé est un motif
de noms de fichiers, remplacé, comme dans bash, par les noms triés qu'il
désigne : `cp *.tmp /tmp`. Un `.` initial doit être désigné par un `.`,
et un motif sans correspondance reste tel qu'il est écrit. Les accolades
sont développées d'abord : `a{b,c}` donne `ab ac`, et `{1..5}` compte.
La protection rend chacun de ces caractères littéral, et la valeur d'une
variable n'est jamais lue comme un motif. `shopt -s` active les options
de bash : `nullglob` (aucune correspondance ne donne aucun mot),
`failglob` (l'absence de correspondance est une erreur), `dotglob`,
`nocaseglob`, `globstar` (`**` désigne toute profondeur) et `extglob`
(`?(…)`, `*(…)`, `+(…)`, `@(…)`, `!(…)`) ; `shopt -o -s noglob`
désactive les motifs de noms de fichiers.

Le shell ne prend aucun opérande : l'exécution de scripts ne fait pas
encore partie de sa grammaire.

Sur un terminal, le shell offre un éditeur de ligne interactif :
Haut/Bas parcourent l'historique des commandes, `Ctrl-R` le recherche,
`Ctrl-C` abandonne la ligne en cours, `Ctrl-D` sur une ligne vide
termine la session, et Tab complète les noms de commandes, les chemins
et les références de ressources comme `sys:random`. Un espace de noms
complète ses sélecteurs enregistrés segment par segment
(`state:` → `net/` → `wan/` → `link`). Là où le registre ne connaît que la
forme — un nom d'interface, une ligne d'interruption —, Tab propose les vrais
noms de la machine, ou rien du tout si cette session n'a pas le droit de les
lister. `info:`/`state:`/`stats:` apparaissent comme argument et après `<`, qui en
lit la valeur (`cat < info:mem/physical`), mais jamais après `>` : une telle
ressource ne s'écrit jamais par redirection.

## OPTIONS

- `-h, -?` — afficher l'aide courte de cette commande et quitter.

## EXIT STATUS

- Le code de la commande intégrée `exit`, ou `0` quand le flux d'entrée
  se termine (ou que l'aide courte a été affichée).
- `2` — l'invocation n'a pas été comprise.

## ENVIRONMENT

- `PATH` — les répertoires cherchés après le préfixe fixe de magasins.
- `GLOBIGNORE` — des motifs séparés par `:` dont un motif de noms de
  fichiers écarte les correspondances.
- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`), exportée vers chaque commande lancée.

## SEE ALSO

- `man`
