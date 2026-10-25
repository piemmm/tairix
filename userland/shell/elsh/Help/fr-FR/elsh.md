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

- `cd [-L | -P [-e]] [directory]`, `cd -` — changer le répertoire de
  travail : vers `directory`, vers `HOME` s'il est omis, ou, avec `-`,
  de retour vers `OLDPWD`, en l'affichant. `-L`, le comportement par
  défaut, applique `directory` à `PWD` tel qu'il est écrit tant que
  `PWD` désigne encore le répertoire de travail, si bien que `cd link`
  puis `cd ..` ramène là où se trouve le lien ; `-P` suit d'abord les
  liens, et `PWD` devient le chemin propre du répertoire (`-e` échoue
  alors quand ce chemin ne peut pas être lu). `/..` est `/`, et sous une
  racine d'alias (`Home:/`), un `..` ne remonte jamais au-dessus d'elle.
- `pwd [-L | -P]` — afficher le répertoire de travail : `PWD` tant qu'il
  le désigne encore (`-L`, par défaut), ou le chemin propre du
  répertoire (`-P`).
- `echo ...` — afficher ses opérandes.
- `export NAME=value`, `unset NAME` — modifier l'environnement exporté.
- `jobs`, `fg`, `bg` — contrôle des tâches.
- `ulimit [-a] [-H | -S] [resource [value]]` — lire et imposer des
  limites de ressources ; `ulimit -n [value]` lit ou règle la limite
  de fichiers ouverts.
- `umask [-S] [mode]` — afficher le masque de création des modes de
  fichiers (`0022`), ou avec `-S` les permissions qu'il laisse
  (`u=rwx,g=rx,o=rx`), ou le définir : un `mode` octal est le masque
  lui-même, et un mode symbolique (`g-w`, `u=rwx,g=rx,o=`) modifie les
  permissions qu'il laisse. Chaque commande lancée ensuite crée ses
  fichiers sous ce masque, et une redirection crée son fichier en `0666`
  moins ce masque.
- `elevate <user> <program> [argument ...]` — exécuter une seule
  commande, avec les arguments donnés, ré-authentifiée via le
  superviseur de connexion de la console.
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

Sur un terminal, le shell offre un éditeur de ligne interactif avec les
touches familières de bash et zsh : Haut/Bas (ou `Ctrl-P`/`Ctrl-N`)
parcourent l'historique des commandes de la session, `Ctrl-R` y lance
une recherche incrémentale, Gauche/Droite/Début/Fin et
`Ctrl-A`/`Ctrl-E` déplacent le curseur,
`Ctrl-K`/`Ctrl-U`/`Ctrl-W`/`Ctrl-Y` coupent et recollent, `Ctrl-T`
intervertit deux caractères, `Ctrl-L` redessine sur un écran effacé,
`Ctrl-C` abandonne la ligne en cours d'édition, et `Ctrl-D` sur une
ligne vide termine la session. Tab complète le mot sous le curseur :
noms de commandes (commandes intégrées et applications de commande
installées), chemins de fichiers et — pour une cible de redirection ou
un mot en forme de référence — références de ressources comme
`sys:random`. Un espace de noms complète ses sélecteurs enregistrés
segment par segment (`state:` → `net/` → `wan/` → `link`) ; là où le
registre ne connaît que la forme — un nom d'interface, une ligne
d'interruption —, Tab propose les vrais noms de la machine, ou rien du
tout si cette session n'a pas le droit de les lister.
`info:`/`state:`/`stats:` apparaissent comme arguments et après `<`,
qui en lit la valeur (`cat < info:mem/physical`), mais jamais après
`>` : une telle ressource ne s'écrit jamais par redirection. Une entrée
venant d'un tube ou d'un script contourne l'éditeur et se comporte de
la même façon avec ou sans terminal.

Le shell ne prend aucun opérande : l'exécution de scripts ne fait pas
encore partie de sa grammaire.

## OPTIONS

- `-h, -?` — afficher l'aide courte de cette commande et quitter.

## EXIT STATUS

- Le code de la commande intégrée `exit`, ou `0` quand le flux d'entrée
  se termine (ou que l'aide courte a été affichée).
- `2` — l'invocation n'a pas été comprise.

## ENVIRONMENT

- `HOME` — là où va `cd` sans opérande.
- `PWD`, `OLDPWD` — le répertoire de travail tel que `cd` l'a atteint,
  et le précédent ; `cd` définit les deux. Un `PWD` avec lequel le shell
  démarre est conservé tant qu'il désigne le répertoire de travail, et
  devient sinon le chemin propre de ce répertoire.
- `PATH` — les répertoires cherchés après le préfixe fixe de magasins.
- `GLOBIGNORE` — des motifs séparés par `:` dont un motif de noms de
  fichiers écarte les correspondances. Tant que la variable n'est pas vide,
  un motif de noms de fichiers désigne aussi les noms commençant par `.`, si
  bien que `rm *` supprime alors aussi `.profile`.
- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`), exportée vers chaque commande lancée.

## SEE ALSO

- `man`
