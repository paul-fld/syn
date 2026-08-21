-- Les plans v1 confondaient « message distant recensé » et « contenu analysé ».
-- Ils ne doivent jamais rester confirmables après l'installation de l'audit v2.
UPDATE mail_cleanup_plans
SET status = 'superseded'
WHERE status = 'pending';

UPDATE actions_log
SET status = 'rejected',
    result = 'Plan remplacé par l’audit complet de boîte mail v2.'
WHERE tool = 'mail.cleanup.apply'
  AND status = 'awaiting_confirmation';
