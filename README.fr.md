# Po

Un harnais d'agent pour le terminal, pensé pour les **modèles locaux**, qui marche tout aussi
bien avec les modèles du cloud.

[English](README.md)

Po fait ce que fait un harnais d'agent — boucle d'agent, appels d'outils, permissions, gestion
du contexte, MCP — et trois choses que la plupart ne font pas :

1. **Il fait tourner les modèles locaux, il ne se contente pas de leur parler.** Découverte du
   matériel, paramètres de lancement, démarrage et arrêt du serveur, bascule de modèle,
   libération de la VRAM. Il lit ce que chaque serveur local sert vraiment (fenêtre de contexte,
   slots parallèles, lecture d'images) au lieu de le deviner.
2. **Il arbitre entre coût et qualité par politique.** L'orchestrateur délègue à des
   sous-agents selon la nature de la tâche et un mode de budget changeable en pleine session.
3. **Il s'adapte au terminal au lieu de le supposer.** Capacités interrogées, rendu dégradé
   proprement, terminal toujours rendu intact — Windows, Linux, macOS, tmux, SSH, Termux, et
   téléphone via Termius.

## Installer

```bash
cargo build --release
./target/release/po
```

Sous Windows, `pwsh -File scripts/install.ps1` construit Po et l'installe dans `~/.cargo/bin`
(déjà dans le PATH) : `po` se lance ensuite depuis n'importe quel dossier et n'importe quel
terminal. `po --version` donne le commit installé.

Aucune configuration n'est nécessaire. Au démarrage, Po cherche les serveurs d'inférence locaux
sur les ports usuels (llama.cpp, Strata, Ollama, LM Studio, vLLM) et les clés d'API présentes
dans l'environnement (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GROQ_API_KEY`,
`OPENROUTER_API_KEY`, `DEEPSEEK_API_KEY`).

```bash
po                          # session interactive
po -p "ta question"         # une seule requête, utilisable dans un tube
po --mode save-max-tokens   # privilégier l'économie
po --continue               # reprendre la dernière session de ce dossier
po local                    # ce que la machine peut faire tourner
po doctor                   # ce que Po a compris du terminal
po sessions                 # sessions enregistrées
```

## Langue

Toute l'interface existe en **français et en anglais**. Po suit la langue du système la
première fois, puis retient votre choix. `/langue fr` ou `/langue en` (ou les réglages, F2)
change l'écran, les commandes et l'aide tout de suite, même en plein tour, sans redémarrer.

## En session

| Saisie | Effet |
|---|---|
| `/` | liste des commandes : ↑↓ pour parcourir, Entrée pour choisir, Entrée encore pour lancer |
| `@chemin` | joint un fichier (ou une image, pour un modèle qui voit) |
| `!commande` | exécute la commande tout de suite ; sa sortie est jointe à la demande suivante |
| F2 ou `/parametres` | panneau des réglages — utilisable aussi pendant que le modèle travaille |
| Ctrl+K | chercher une commande ou un réglage |
| Maj+Tab | mode de travail : plan (lecture seule), builder (accord avant les actions sensibles), bypass |
| Ctrl+O, ou toucher un bloc replié | déplier raisonnements, appels d'outils, consignes de sous-agents |
| Échap | fermer ce qui est ouvert ; deux fois pour arrêter l'agent et ses sous-agents |
| F3 | dictée vocale, si elle est activée |
| `/aide` | toutes les commandes |

Un réglage changé pendant un tour s'applique au prochain point sûr — juste après les appels
d'outils en cours, avant l'étape suivante du modèle — sinon à la fin du tour. Il n'est jamais
nécessaire de relancer une session pour changer quelque chose.

Le titre de la session est rédigé par le modèle après le premier échange. Sous la saisie, une
ligne d'informations donne l'heure, le contexte utilisé et les vitesses de prefill et de
génération en direct. Sous tmux, défiler à la molette ou au doigt (Termius) demande
`set -g mouse on` dans `~/.tmux.conf` ; Po le signale s'il manque.

### Sous-agents

L'outil `task` confie une tâche à un sous-agent. Six rôles sont fournis — `explorer`,
`planner`, `builder`, `verifier`, `reviewer`, `summarizer` — et vous pouvez ajouter les vôtres
par projet (`.po/agents/<nom>.md`) ou pour toutes vos sessions (`%APPDATA%/po/agents` sous
Windows, `~/.config/po/agents` ailleurs). `/agents creer <description>` en fait rédiger un par le
modèle de la session.

- **Suivi en direct.** Les sous-agents en cours forment un paquet de cartes dans un coin, chacun
  de sa couleur, avec ce qu'il dit faire. Un toucher ouvre sa carte en grand : toute sa
  conversation — réflexion, outils, texte — mise en page comme la conversation principale. On
  change de carte avec `‹ préc. · suiv. ›`, les flèches, ou un balayage là où le terminal le
  transmet.
- **Parallélisme** : il suit les slots annoncés par le serveur (`/props`). Avec un slot,
  l'agent principal attend son sous-agent ; avec N, N sous-agents tournent ensemble.
- **Cache préservé** : sur le serveur de la session, un sous-agent reprend à l'identique le
  prompt système et les outils de l'agent principal, ce qui garde le cache du serveur.

### Médias

Po demande à chaque serveur ce que son modèle sait lire (`input_modalities`, ou `/props`).
Quand il voit les images, `@image.png` en joint une et l'outil `read` peut en ouvrir lui-même ;
la vidéo et le son passent vers les serveurs qui les annoncent.

### Dictée vocale (facultative)

Désactivée par défaut. `/voix on`, puis `/voix installer` télécharge
[whisper.cpp](https://github.com/ggml-org/whisper.cpp) et un modèle. Un bouton **dicter**
apparaît sous la saisie (ou F3) : un toucher enregistre, un second transcrit dans la saisie —
rien ne part avant Entrée. Processeur (`small`, environ 3,5 s par phrase) ou carte graphique
(`turbo`, environ 2 s) — `/voix moteur gpu` choisit la carte la plus libre.

### Po Labs

**Po Labs** vous permet de façonner Po simplement en en parlant. Désactivé par défaut —
`/labs on`, ou en tête des réglages. Une fois activé, le modèle repère dans vos messages ce qui
concerne Po lui-même (un texte mal affiché, une couleur à changer, un bug, une idée), le note à
part du reste de votre demande et vous le montre dans une carte ; `/labs` liste ce qui a été
noté. Étapes suivantes : essayer ces changements en direct sur votre propre Po, puis les
proposer à tous, à votre nom. Voir [docs/po-labs.md](docs/po-labs.md).

### Prise de contrôle

Chaque session plein écran ouvre un serveur MCP local (127.0.0.1, jeton secret dans l'adresse).
`/controle` donne l'adresse. Un autre agent — un autre Po, Claude Code, tout client MCP — peut
alors voir l'écran, lire la conversation, changer un réglage, envoyer un message, ou relancer Po
sur la même session avec le binaire tout juste installé (`/redemarrer`).

## Modes de budget

| Mode | Orchestrateur | Sous-agents |
|---|---|---|
| `max-quality` | meilleur modèle | meilleur modèle |
| `balanced` | cloud | local pour recherche, résumé, classification |
| `save-max-tokens` | cloud | le plus petit modèle local au-dessus du plancher de la tâche |
| `offline` | local | local |

Ctrl+G fait tourner les modes en pleine conversation. Chaque tâche a un **plancher de
qualité** que l'économie ne franchit jamais : une édition de code n'atterrit jamais sur un
modèle de classification.

## Modèles locaux

Po lit l'en-tête GGUF pour connaître la géométrie réelle du modèle, mesure la VRAM **libre**,
en déduit `-ngl`, `-c`, `-np` et le type de cache, lance `llama-server` et attend qu'il réponde
vraiment. Les compromis sont annoncés, jamais subis. Un serveur inactif rend sa mémoire ; un
serveur lancé par Po s'arrête avec Po. Le temps de calcul local est mesuré et affiché : un
modèle local est gratuit en euros, pas en secondes.

## Configuration

Facultative, dans `po.toml` (dossier courant, `.po/`, ou dossier de configuration de
l'utilisateur) — voir [docs/configuration.md](docs/configuration.md) (en anglais) pour toutes
les options et variables d'environnement.

## Sûreté

- Les outils de fichiers sont **confinés à l'espace de travail** ; un chemin qui en sort est
  refusé, pas réécrit.
- Les commandes sont classées **d'après leur texte réel** : `git status` passe sans déranger,
  `ls && rm -rf ~` demande l'accord, une poignée de motifs irréversibles sont refusés d'emblée.
  Ce n'est pas un bac à sable — un shell reste un shell — mais votre accord n'est jamais
  contourné.
- Les outils MCP demandent toujours l'accord : c'est du code tiers.
- Le texte venu du modèle, des outils ou de serveurs tiers est neutralisé pour ne pas pouvoir
  repeindre le terminal.

## Documentation

- [Premiers pas](docs/getting-started.md) (en anglais)
- [Configuration](docs/configuration.md) (en anglais)
- [Po Labs](docs/po-labs.md) (en anglais)
- [Contribuer](CONTRIBUTING.md) · [Sécurité](SECURITY.md)

## Licence

Au choix, [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE-APACHE). Sauf mention contraire de votre
part, toute contribution que vous proposez est placée sous ces deux mêmes licences.
